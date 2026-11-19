//! vesper-settings: the Settings window (Mod+,). A normal window with a page list on the left; every change is written to
//! `~/.config/vesper/config.toml` (or the colour settings) and the compositor applies it live.

mod model;
mod ui;

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use model::{Act, Page, Snapshot, Val};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        calloop_wayland_source::WaylandSource,
        client::{
            globals::registry_queue_init,
            protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
            Connection, QueueHandle,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use ui::{DropState, EditState, Fonts, Hit, Ui};
use vesper_config::theme::Palette;
use vesper_gfx::Canvas;
use vesper_ipc::State;

const BTN_LEFT: u32 = 0x110;

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    loop_handle: smithay_client_toolkit::reexports::calloop::LoopHandle<'static, App>,

    exit: bool,
    configured: bool,
    size: (i32, i32),
    scale: i32,
    fonts: Fonts,
    pal: Palette,
    palette_stamp: Option<std::time::SystemTime>,

    nav: usize,
    tab: usize,
    scroll: i32,
    snap: Snapshot,
    page: Page,
    hits: Vec<(ui::Rect, Hit)>,
    content_height: i32,
    edit: Option<EditState>,
    drop: Option<DropState>,
    status: String,
    status_until: Option<Instant>,
    pointer_pos: (f64, f64),
    hover: Option<usize>,
    stamps: (Option<std::time::SystemTime>, Option<std::time::SystemTime>),
    ipc_state: Option<State>,
    /// A slider being dragged: its hit rectangle and range.
    slider: Option<(ui::Rect, String, f64, f64, bool, i32)>,
    /// A monitor box being dragged in the arrangement map: its index, the map area, scale and centre, and where inside the monitor the pointer grabbed it (logical px).
    mon_drag: Option<(usize, ui::Rect, f64, (f64, f64), (f64, f64))>,
    slider_saved: Instant,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut args = std::env::args().skip(1);
    let mut start_page = String::from("display");
    let mut start_tab = 0usize;
    while let Some(a) = args.next() {
        if a == "--page" {
            start_page = args.next().unwrap_or_default();
        } else if a == "--tab" {
            start_tab = args.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        }
    }

    let config = vesper_config::Config::load();
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;

    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let xdg_shell = XdgShell::bind(&globals, &qh).map_err(|e| anyhow!("xdg-shell: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;
    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("Settings");
    window.set_app_id("vesper-settings");
    window.set_min_size(Some((560, 420)));
    window.commit();

    let fonts = Fonts::new(&config.bar.font, 1.0);
    let nav = model::PAGES.iter().position(|p| p.0 == start_page).unwrap_or(0);
    let snap = Snapshot::load(None);
    let page = model::build(model::PAGES[nav].0, &snap);
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(1040 * 720 * 4, &shm)?,
        shm,
        window,
        keyboard: None,
        pointer: None,
        loop_handle: handle.clone(),
        exit: false,
        configured: false,
        size: (1040, 720),
        scale: 1,
        fonts,
        pal: Palette::load(),
        palette_stamp: Palette::stamp(),
        nav,
        tab: start_tab,
        scroll: 0,
        snap,
        page,
        hits: Vec::new(),
        content_height: 0,
        edit: None,
        drop: None,
        status: String::new(),
        status_until: None,
        pointer_pos: (-1.0, -1.0),
        hover: None,
        stamps: (vesper_config::Config::stamp(), vesper_config::theme::ThemeSettings::stamp()),
        ipc_state: None,
        slider: None,
        mon_drag: None,
        slider_saved: Instant::now(),
    };

    // the compositor's view of the monitors
    let (tx, rx) = channel::channel::<State>();
    std::thread::spawn(move || {
        let Ok(stream) = UnixStream::connect(vesper_ipc::socket_path()) else { return };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if let Ok(state) = serde_json::from_str::<State>(&line) {
                if tx.send(state).is_err() {
                    return;
                }
            }
        }
    });
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg(state) = event {
                app.ipc_state = Some(state);
                app.reload();
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    // config edits made elsewhere (or by us), new colours, expiring status text
    handle
        .insert_source(Timer::from_duration(Duration::from_millis(300)), |_, _, app| {
            app.poll();
            TimeoutAction::ToDuration(Duration::from_millis(300))
        })
        .map_err(|e| anyhow!("{e}"))?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

impl App {
    fn k(&self) -> f32 {
        self.scale.max(1) as f32
    }

    /// Re-read everything and rebuild the current page.
    fn reload(&mut self) {
        self.snap = Snapshot::load(self.ipc_state.clone());
        self.page = model::build(model::PAGES[self.nav].0, &self.snap);
        self.stamps = (vesper_config::Config::stamp(), vesper_config::theme::ThemeSettings::stamp());
        self.redraw();
    }

    fn poll(&mut self) {
        let stamps = (vesper_config::Config::stamp(), vesper_config::theme::ThemeSettings::stamp());
        if stamps != self.stamps {
            self.reload();
        }
        let pal_stamp = Palette::stamp();
        if pal_stamp != self.palette_stamp {
            self.palette_stamp = pal_stamp;
            self.pal = Palette::load();
            self.redraw();
        }
        if self.status_until.is_some_and(|t| t <= Instant::now()) {
            self.status.clear();
            self.status_until = None;
            self.redraw();
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_until = Some(Instant::now() + Duration::from_secs(3));
    }

    fn save(&mut self, path: &str, val: Val) {
        // the Profiles page types a name or a path: these are actions, not settings
        if let (true, Val::Str(text)) = (path.starts_with("profile.") || path.starts_with("plugin."), &val) {
            let result = match path {
                "profile.save.+" => vesper_config::profiles::save(text).map(|n| format!("Saved as {n}")),
                "profile.import.+" => vesper_config::profiles::import_file(text).map(|n| format!("Imported as {n}: apply it to use it")),
                "plugin.install.+" => vesper_config::plugins::install(text).map(|n| format!("Installed {n}: turn it on in the list")),
                _ => Err("Unknown action".to_string()),
            };
            self.say(result.unwrap_or_else(|e| e));
            self.reload();
            return;
        }
        match model::apply(path, &val) {
            Ok(()) => self.say("Saved"),
            Err(err) => self.say(format!("Could not save: {err}")),
        }
        self.reload();
    }

    fn commit_edit(&mut self) {
        let Some(e) = self.edit.take() else { return };
        let val = match e.numeric {
            Some((min, max, int)) => match model::parse_number(&e.text, min, max, int) {
                Some(v) => v,
                None => {
                    self.say("That is not a number");
                    self.redraw();
                    return;
                }
            },
            None => Val::Str(e.text.trim().to_owned()),
        };
        // an empty text box goes back to the default
        let val = if matches!(&val, Val::Str(s) if s.is_empty()) && !e.path.ends_with(".+") { Val::Unset } else { val };
        self.save(&e.path, val);
    }

    fn act(&mut self, act: Act) {
        let spawn = |prog: &str, args: &[&str]| {
            let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(prog)));
            let bin = sibling.filter(|p| p.is_file()).map(|p| p.into_os_string()).unwrap_or_else(|| prog.into());
            match std::process::Command::new(bin).args(args).spawn() {
                Ok(mut c) => {
                    std::thread::spawn(move || {
                        let _ = c.wait();
                    });
                    true
                }
                Err(_) => false,
            }
        };
        match act {
            Act::OpenPicker => {
                spawn("vesper-wallpaper-picker", &[]);
            }
            Act::OpenThemeEditor => {
                spawn("vesper-wallpaper-picker", &["--theme"]);
            }
            Act::OpenNotifications => {
                spawn("vesper-notify", &["center"]);
            }
            Act::OpenConfigFile => {
                let f = vesper_config::Config::user_file();
                if !f.exists() {
                    let _ = vesper_config::Config::edit(|_| {});
                }
                if !spawn("xdg-open", &[&f.to_string_lossy()]) {
                    self.say("xdg-open is not installed");
                }
            }
            Act::ClearBarSplits => {
                if let Err(e) = vesper_config::Config::unset("bar.splits") {
                    self.say(e.to_string());
                }
                self.reload();
            }
            Act::BarItem(id, dir) => {
                let bar = vesper_config::Config::load().bar;
                let mut layout = model::bar_layout(bar.left_items, bar.middle_items, bar.right_items);
                model::apply_bar_move(&mut layout, id, dir);
                let arr = |v: Vec<String>| toml::Value::Array(v.into_iter().map(toml::Value::String).collect());
                let [left, middle, right] = layout;
                let right_rtl: Vec<String> = right.into_iter().rev().collect();
                for (key, list) in [("bar.left_items", left), ("bar.middle_items", middle), ("bar.right_items", right_rtl)] {
                    if let Err(e) = vesper_config::Config::set(key, arr(list)) {
                        self.say(e.to_string());
                    }
                }
                self.reload();
            }
            Act::MakePrimary(name) => self.save("display.primary", Val::Str(name)),
            Act::ClearOutput(name) => {
                if let Err(e) = model::clear_output(&name) {
                    self.say(e);
                }
                self.reload();
            }
            Act::RemoveListItem(key, i) => {
                if let Err(e) = model::remove_list_item(key, i) {
                    self.say(e);
                }
                self.reload();
            }
            Act::PluginToggle(id) => {
                let on = !vesper_config::plugins::discover(&vesper_config::Config::load().plugins.enabled).iter().any(|p| p.id == id && p.enabled);
                match vesper_config::plugins::set_enabled(&id, on) {
                    Ok(()) => self.say(format!("{id} is {}", if on { "on" } else { "off" })),
                    Err(e) => self.say(e.to_string()),
                }
                self.reload();
            }
            Act::PluginRemove(id) => {
                let _ = vesper_config::plugins::set_enabled(&id, false);
                match vesper_config::plugins::remove(&id) {
                    Ok(()) => self.say(format!("Removed {id}")),
                    Err(e) => self.say(e),
                }
                self.reload();
            }
            Act::OpenPluginsFolder => {
                let dir = vesper_config::plugins::user_dir();
                let _ = std::fs::create_dir_all(&dir);
                if !spawn("xdg-open", &[&dir.to_string_lossy()]) {
                    self.say("xdg-open is not installed");
                }
            }
            Act::ProfileApply(name) => {
                match vesper_config::profiles::apply(&name) {
                    Ok(()) => self.say(format!("Applied {name}: the previous settings are kept as Previous")),
                    Err(e) => self.say(e),
                }
                self.reload();
            }
            Act::ProfileShare(name) => match vesper_config::profiles::export_text(&name) {
                Ok(text) => {
                    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
                    let dir = if home.join("Downloads").is_dir() { home.join("Downloads") } else { home };
                    let file = dir.join(format!("vesper-{}.toml", name.replace(' ', "-")));
                    let saved = std::fs::write(&file, &text).is_ok();
                    send(vesper_ipc::Command::Clipboard { text, summary: "Config copied".into(), body: format!("{name}: paste it anywhere to share") });
                    self.say(if saved { format!("Copied to the clipboard and saved to {}", file.display()) } else { "Copied to the clipboard".to_string() });
                }
                Err(e) => self.say(e),
            },
            Act::ProfileDelete(name) => {
                match vesper_config::profiles::delete(&name) {
                    Ok(()) => self.say(format!("Deleted {name}")),
                    Err(e) => self.say(e),
                }
                self.reload();
            }
            Act::Lock => send(vesper_ipc::Command::Lock),
            Act::Suspend => send(vesper_ipc::Command::Suspend),
        }
    }

    fn click(&mut self) {
        let Some(i) = ui::hit_at(&self.hits, self.pointer_pos) else {
            self.commit_edit();
            self.drop = None;
            self.redraw();
            return;
        };
        let hit = self.hits[i].1.clone();
        if !matches!(hit, Hit::Edit { .. }) {
            self.commit_edit();
        }
        match hit {
            Hit::Nav(n) => {
                self.drop = None;
                self.nav = n;
                self.tab = 0;
                self.scroll = 0;
                self.reload();
            }
            Hit::Tab(t) => {
                self.tab = t;
                self.scroll = 0;
                self.drop = None;
                self.redraw();
            }
            Hit::Toggle(path, v) => self.save(&path, Val::Bool(v)),
            Hit::Pick(path, v) => {
                self.drop = None;
                self.save(&path, v);
            }
            Hit::Drop { path, options, cur, anchor } => {
                let same = self.drop.as_ref().is_some_and(|d| d.path == path);
                self.drop = if same { None } else { Some(DropState { path, options, cur, anchor }) };
                self.redraw();
            }
            Hit::Step { path, cur, delta, min, max, int } => {
                let v = (cur + delta).clamp(min, max);
                let v = if int { Val::Int(v.round() as i64) } else { Val::Float((v * 1000.0).round() / 1000.0) };
                self.save(&path, v);
            }
            Hit::Edit { path, initial, numeric } => {
                if self.edit.as_ref().is_some_and(|e| e.path != path) {
                    self.commit_edit();
                }
                self.drop = None;
                if self.edit.is_none() {
                    self.edit = Some(EditState { path, text: initial, numeric });
                }
                self.redraw();
            }
            Hit::Slider { path, min, max, int, pad } => {
                self.slider = Some((self.hits[i].0, path, min, max, int, pad));
                self.slide(true);
            }
            Hit::Monitor { idx, area, scale, center } => {
                if let Some(m) = self.map_mons().and_then(|m| m.get(idx).cloned()) {
                    let (lx, ly) = self.map_logical(area, scale, center);
                    self.mon_drag = Some((idx, area, scale, center, (lx - m.x as f64, ly - m.y as f64)));
                }
            }
            Hit::Act(a) => self.act(a),
            Hit::CloseDrop => {
                self.drop = None;
                self.redraw();
            }
        }
    }

    /// Set the dragged slider from the pointer; saves at most every 60 ms while dragging (and always when `force`).
    fn slide(&mut self, force: bool) {
        let Some((r, path, min, max, int, pad)) = self.slider.clone() else { return };
        if !force && self.slider_saved.elapsed() < Duration::from_millis(60) {
            return;
        }
        self.slider_saved = Instant::now();
        let (x0, w) = (r.0 + pad, (r.2 - 2 * pad).max(1));
        let frac = ((self.pointer_pos.0 - x0 as f64) / w as f64).clamp(0.0, 1.0);
        let v = min + frac * (max - min);
        let val = if int { Val::Int(v.round() as i64) } else { Val::Float((v * 100.0).round() / 100.0) };
        if let Err(err) = model::apply(&path, &val) {
            self.say(format!("Could not save: {err}"));
        }
        self.reload();
    }

    fn key(&mut self, event: KeyEvent) {
        let sym = event.keysym;
        if let Some(e) = self.edit.as_mut() {
            match sym {
                Keysym::Return | Keysym::KP_Enter | Keysym::Tab => self.commit_edit(),
                Keysym::Escape => {
                    self.edit = None;
                    self.redraw();
                }
                Keysym::BackSpace => {
                    e.text.pop();
                    self.redraw();
                }
                _ => {
                    if let Some(t) = event.utf8.as_deref() {
                        if !t.chars().any(|c| c.is_control()) {
                            e.text.push_str(t);
                            self.redraw();
                        }
                    }
                }
            }
            return;
        }
        match sym {
            Keysym::Escape => {
                if self.drop.take().is_some() {
                    self.redraw();
                }
            }
            Keysym::Down | Keysym::j => self.scroll_by(60),
            Keysym::Up | Keysym::k => self.scroll_by(-60),
            Keysym::Page_Down | Keysym::space => self.scroll_by(self.size.1 * self.scale * 3 / 4),
            Keysym::Page_Up => self.scroll_by(-(self.size.1 * self.scale * 3 / 4)),
            Keysym::Home => self.scroll_by(-1_000_000),
            Keysym::End => self.scroll_by(1_000_000),
            _ => {}
        }
    }

    fn scroll_by(&mut self, d: i32) {
        let max = (self.content_height - self.size.1 * self.scale).max(0);
        let new = (self.scroll + d).clamp(0, max);
        if new != self.scroll {
            self.scroll = new;
            self.redraw();
        }
    }

    fn redraw(&mut self) {
        if !self.configured {
            return;
        }
        let k = self.scale.max(1);
        let (w, h) = (self.size.0 * k, self.size.1 * k);
        if w <= 0 || h <= 0 {
            return;
        }
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas::new(canvas, w as u32, h as u32);
        let max = (self.content_height - h).max(0);
        self.scroll = self.scroll.clamp(0, max);
        let drawn = Ui {
            pal: &self.pal,
            fonts: &self.fonts,
            k: k as f32,
            w,
            h,
            page: &self.page,
            nav: self.nav,
            tab: self.tab,
            scroll: self.scroll,
            edit: self.edit.as_ref(),
            drop: self.drop.as_ref(),
            status: &self.status,
            pointer: self.pointer_pos,
        }
        .draw(&mut c);
        self.hits = drawn.hits;
        self.content_height = drawn.content_height;
        self.hover = ui::hit_at(&self.hits, self.pointer_pos);
        let surface = self.window.wl_surface();
        let _ = surface.set_buffer_scale(k);
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            self.window.commit();
        }
    }
}

/// Send one command to the compositor.
fn send(cmd: vesper_ipc::Command) {
    if let Ok(mut s) = UnixStream::connect(vesper_ipc::socket_path()) {
        if let Ok(json) = serde_json::to_string(&cmd) {
            let _ = writeln!(s, "{json}");
        }
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.commit_edit();
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        let w = configure.new_size.0.map(|v| v.get() as i32).unwrap_or(self.size.0);
        let h = configure.new_size.1.map(|v| v.get() as i32).unwrap_or(self.size.1);
        self.size = (w.max(300), h.max(300));
        self.configured = true;
        self.redraw();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, new: i32) {
        if new != self.scale && new > 0 {
            self.scale = new;
            self.fonts = self.fonts.rescale(new as f32);
            self.redraw();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if &e.surface != self.window.wl_surface() {
                continue;
            }
            let k = self.k() as f64;
            let pos = (e.position.0 * k, e.position.1 * k);
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = pos;
                    if self.slider.is_some() {
                        self.slide(false);
                        continue;
                    }
                    if self.mon_drag.is_some() {
                        self.drag_monitor();
                        continue;
                    }
                    let over = ui::hit_at(&self.hits, pos);
                    // redraw only when what is under the pointer changes (hover highlight)
                    if over != self.hover {
                        self.redraw();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_pos = (-1.0, -1.0);
                    self.redraw();
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    self.pointer_pos = pos;
                    self.click();
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.pointer_pos = pos;
                    if self.slider.is_some() {
                        self.slide(true);
                        self.slider = None;
                    }
                    if self.mon_drag.take().is_some() {
                        self.drop_monitor();
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    self.scroll_by((vertical.absolute * k) as i32);
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), Box::new(|app, _, event| app.key(event))).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl App {
    /// The monitors of the arrangement map on this page, if it has one.
    fn map_mons(&mut self) -> Option<&mut Vec<model::MapMon>> {
        self.page.sections.iter_mut().flat_map(|s| s.rows.iter_mut()).find_map(|r| match &mut r.ctl {
            model::Ctl::MonitorMap { mons, .. } => Some(mons),
            _ => None,
        })
    }

    /// The pointer position in logical monitor pixels, for a map drawn in `area` at `scale` around `center`.
    fn map_logical(&self, area: ui::Rect, scale: f64, center: (f64, f64)) -> (f64, f64) {
        let (cx, cy) = (area.0 as f64 + area.2 as f64 / 2.0, area.1 as f64 + area.3 as f64 / 2.0);
        (center.0 + (self.pointer_pos.0 - cx) / scale, center.1 + (self.pointer_pos.1 - cy) / scale)
    }

    /// Move the dragged monitor box with the pointer (nothing is saved until it is dropped).
    fn drag_monitor(&mut self) {
        let Some((idx, area, scale, center, grab)) = self.mon_drag else { return };
        let (lx, ly) = self.map_logical(area, scale, center);
        let Some(mons) = self.map_mons() else { return };
        let (x, y) = model::snap_position(mons, idx, (lx - grab.0).round() as i32, (ly - grab.1).round() as i32);
        mons[idx].x = x;
        mons[idx].y = y;
        self.redraw();
    }

    /// The monitor was dropped: save the arrangement.
    fn drop_monitor(&mut self) {
        let Some(mons) = self.map_mons().map(|m| m.clone()) else { return };
        match model::apply_layout(&mons) {
            Ok(()) => self.say("Monitor arrangement saved"),
            Err(e) => self.say(format!("Could not save: {e}")),
        }
        self.reload();
    }
}

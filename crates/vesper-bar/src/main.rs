//! vesper-bar: the Vesper top bar. A wlr-layer-shell client that draws into shared memory.
//!
//! Left: workspace pills (click to switch). Centre: focused window. Right: clock.

mod bt;
mod cmd;
mod dbus;
mod drives;
mod draw;
mod icons;
mod plugin;
mod popup;
mod quick;
mod shape;
mod sys;

use draw::Hit;

use std::{
    io::{BufRead, BufReader, Write},
    num::NonZeroU32,
    os::unix::net::UnixStream,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_keyboard, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel::{self, Channel},
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
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_ipc::{Command, State};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

struct PopupWin {
    layer: LayerSurface,
    w: u32,
    h: u32,
    configured: bool,
    pop: popup::Popup,
}

struct Bar {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    compositor: CompositorState,
    layer_shell: LayerShell,
    /// The open popup (calendar, volume, ...) and its overlay surface.
    popup: Option<PopupWin>,
    popup_pool: Option<SlotPool>,
    popup_hover: Option<popup::PHit>,
    osd: Option<OsdWin>,
    osd_pool: Option<SlotPool>,
    last_osd_seq: Option<u32>,
    plugin_items: Vec<plugin::Item>,
    plugin_runner: plugin::Runner,
    popup_drag: Option<popup::Slider>,
    dbus_tx: channel::Sender<dbus::Dbus>,
    media_started: bool,
    start_seen: Option<u32>,
    last_ptt_live: Option<bool>,
    /// The lowest battery warning already shown (100 = none), so each one comes once per discharge.
    battery_warned: u32,
    battery_suspended: bool,
    tray_started: bool,
    /// Set in a copy of the bar that runs on one extra monitor: the output name. The first bar starts and stops these.
    secondary: Option<String>,
    children: std::collections::HashMap<String, (std::process::Child, std::time::Instant)>,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    qh: QueueHandle<Bar>,

    width: u32,
    height: u32,
    configured: bool,
    exit: bool,

    state: State,
    clock: String,
    renderer: draw::Renderer,
    /// Clickable regions from the last draw.
    hits: Vec<(i32, i32, Hit)>,
    hover: Option<Hit>,
    poller: sys::Poller,
    palette_stamp: Option<std::time::SystemTime>,
    /// The shapes last reported to the compositor.
    last_shapes: Vec<[i32; 4]>,
    config_stamp: Option<std::time::SystemTime>,
    ipc: UnixStream,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();

    // --- IPC: connect and read state updates on a thread
    let path = vesper_ipc::socket_path();
    let ipc = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    let reader = ipc.try_clone()?;
    let (tx, rx): (channel::Sender<Option<State>>, Channel<Option<State>>) = channel::channel();
    let (vol_tx, vol_rx) = channel::channel::<()>();
    let (dbus_tx, dbus_rx) = channel::channel::<dbus::Dbus>();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if let Ok(state) = serde_json::from_str::<State>(&line) {
                if tx.send(Some(state)).is_err() {
                    return;
                }
            }
        }
        let _ = tx.send(None); // compositor went away
    });

    // --- Wayland
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let style = draw::Style::from_config(&config.bar);
    let height = style.surface_height() as u32;
    let surface = compositor.create_surface(&qh);
    let secondary = std::env::var("VESPER_BAR_OUTPUT").ok().filter(|s| !s.is_empty());
    let layer_name = match &secondary {
        Some(out) => format!("vesper-bar@{out}"),
        None => "vesper-bar".to_string(),
    };
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some(layer_name.as_str()), None);
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, height);
    layer.set_exclusive_zone(height as i32);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();

    let mut bar = Bar {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(1920 * height as usize * 4, &shm)?,
        shm,
        compositor,
        layer_shell,
        popup: None,
        popup_pool: None,
        popup_hover: None,
        osd: None,
        osd_pool: None,
        last_osd_seq: None,
        plugin_items: Vec::new(),
        plugin_runner: Default::default(),
        popup_drag: None,
        dbus_tx,
        media_started: false,
        start_seen: None,
        last_ptt_live: None,
        battery_warned: 100,
        battery_suspended: false,
        tray_started: false,
        secondary: std::env::var("VESPER_BAR_OUTPUT").ok().filter(|s| !s.is_empty()),
        children: Default::default(),
        layer,
        pointer: None,
        keyboard: None,
        qh: qh.clone(),
        width: 0,
        height,
        configured: false,
        exit: false,
        state: State::default(),
        clock: String::new(),
        renderer: draw::Renderer::new(&config.bar.font, config.bar.font_size, config.bar.height.max(16), config.bar.icon_size),
        hits: Vec::new(),
        hover: None,
        poller: sys::Poller::new(),
        palette_stamp: vesper_config::theme::Palette::stamp(),
        last_shapes: Vec::new(),
        config_stamp: vesper_config::Config::stamp(),
        ipc,
    };

    bar.renderer.set_opacity(config.bar.opacity);
    bar.renderer.volume_max = if config.media.volume_boost { 150 } else { 100 };
    bar.renderer.style = style;
    bar.renderer.splits = config.bar.splits.clone();
    let all_items = |b: &vesper_config::Bar| -> Vec<String> { [b.left_items.clone(), b.middle_items.clone(), b.right_items.clone()].concat() };
    bar.ensure_dbus(&all_items(&config.bar));
    bar.sync_plugins(&config.bar, &config.plugins.enabled);
    bar.poller.volume_events = sys::watch_volume(move || {
        let _ = vol_tx.send(());
    });
    bar.poller.poll();
    let mut event_loop: EventLoop<Bar> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    KEY_LOOP.with(|k| *k.borrow_mut() = Some(handle.clone()));
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(rx, |event, _, bar| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Some(state) => {
                        bar.poller.sys.kb = (state.kb_layouts > 1).then(|| state.kb_layout.clone());
                        if bar.last_ptt_live != Some(state.ptt_live) {
                            if bar.last_ptt_live.is_some() || state.ptt_live {
                                bar.poller.sys.mic = Some(state.ptt_live);
                            }
                            bar.last_ptt_live = Some(state.ptt_live);
                        }
                        // the volume or brightness keys were pressed: show the new level on screen shortly after
                        if let Some(prev) = bar.last_osd_seq {
                            if bar.secondary.is_none() && prev != state.osd_seq {
                                let kind: &'static str = if state.osd_kind == "brightness" { "brightness" } else { "volume" };
                                if let Some(handle) = KEY_LOOP.with(|k| k.borrow().clone()) {
                                    let _ = handle.insert_source(
                                        smithay_client_toolkit::reexports::calloop::timer::Timer::from_duration(Duration::from_millis(150)),
                                        move |_, _, bar: &mut Bar| {
                                            bar.show_osd(kind, true);
                                            smithay_client_toolkit::reexports::calloop::timer::TimeoutAction::Drop
                                        },
                                    );
                                }
                            }
                        }
                        bar.last_osd_seq = Some(state.osd_seq);
                        let seq = state.start_menu;
                        let pressed = bar.secondary.is_none() && bar.start_seen.is_some_and(|s| s != seq);
                        bar.start_seen = Some(seq);
                        // each monitor has its own workspaces: this bar shows (and controls) the ones of its own monitor
                        let mut state = state;
                        let mine = bar.secondary.clone().or_else(|| state.outputs.iter().find(|o| o.primary).map(|o| o.name.clone()));
                        if let Some(m) = state.monitors.iter().find(|m| Some(&m.output) == mine.as_ref()).cloned() {
                            state.workspaces = m.workspaces;
                            state.active = m.active;
                            state.focused = m.focused;
                            state.minimized = m.minimized;
                            state.minimized_ws = m.minimized_ws;
                        }
                        bar.state = state;
                        bar.redraw();
                        bar.sync_secondary_bars();
                        if pressed {
                            bar.open_start_menu();
                        }
                    }
                    None => bar.exit = true,
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(dbus_rx, |event, _, bar| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    dbus::Dbus::Media(m) => bar.poller.sys.media = m,
                    dbus::Dbus::Tray(t) => bar.poller.sys.tray = t,
                    dbus::Dbus::Bt => bar.redraw_popup(),
                    dbus::Dbus::Plugin(id, text) => {
                        if bar.poller.sys.plugin.get(id) != Some(&text) {
                            bar.poller.sys.plugin.insert(id, text);
                        }
                    }
                    dbus::Dbus::Wifi(res) => match res {
                        Ok(()) => bar.close_popup(),
                        Err(m) => {
                            if let Some(pr) = bar.popup.as_mut().and_then(|p| p.pop.prompt.as_mut()) {
                                pr.busy = false;
                                pr.error = Some(m);
                            }
                            bar.redraw_popup();
                        }
                    },
                }
                bar.redraw();
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    // the sound server says the volume changed
    handle
        .insert_source(vol_rx, |event, _, bar| {
            if let channel::Event::Msg(()) = event {
                let before = (bar.poller.sys.volume.clone(), bar.poller.sys.mic);
                bar.poller.refresh_volume();
                if (bar.poller.sys.volume.clone(), bar.poller.sys.mic) != before {
                    bar.redraw_popup();
                    bar.redraw();
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    // Wake up every second; only redraw when the displayed time changed.
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, bar| {
            let now = bar.renderer.style.clock_text(chrono::Local::now());
            let sys_changed = bar.poller.poll();
            bar.check_battery();
            if bar.last_ptt_live == Some(true) {
                bar.poller.sys.mic = Some(true); // held: do not let a poll show the old state
            }
            if bar.popup.as_ref().is_some_and(|p| p.pop.kind == popup::Kind::Start) {
                bar.redraw_popup();
            }
            if now != bar.clock || sys_changed {
                bar.clock = now;
                bar.redraw();
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow!("{e}"))?;

    // New colours from matugen: look five times a second, it is only a file timestamp.
    handle
        .insert_source(Timer::from_duration(Duration::from_millis(500)), |_, _, bar| {
            let cfg_stamp = vesper_config::Config::stamp();
            if cfg_stamp != bar.config_stamp {
                bar.config_stamp = cfg_stamp;
                let cfg = vesper_config::Config::load();
                let mut changed = bar.renderer.set_opacity(cfg.bar.opacity);
                let vm = if cfg.media.volume_boost { 150 } else { 100 };
                if bar.renderer.volume_max != vm {
                    bar.renderer.volume_max = vm;
                    changed = true;
                }
                if bar.renderer.splits != cfg.bar.splits {
                    bar.renderer.splits = cfg.bar.splits.clone();
                    changed = true;
                }
                changed |= bar.renderer.set_icon_size(cfg.bar.height.max(16), cfg.bar.icon_size);
                let style = draw::Style::from_config(&cfg.bar);
                bar.ensure_dbus(&[style.left_items.clone(), style.middle_items.clone(), style.right_items.clone()].concat());
                bar.sync_plugins(&cfg.bar, &cfg.plugins.enabled);
                changed |= true;
                if format!("{:?}", style) != format!("{:?}", bar.renderer.style) {
                    let new_h = style.surface_height() as u32;
                    bar.renderer.style = style;
                    if new_h != bar.height {
                        bar.layer.set_size(0, new_h);
                        bar.layer.set_exclusive_zone(new_h as i32);
                        bar.layer.commit();
                    }
                    changed = true;
                }
                if changed {
                    bar.redraw();
                }
            }
            let stamp = vesper_config::theme::Palette::stamp();
            if stamp != bar.palette_stamp {
                bar.palette_stamp = stamp;
                if bar.renderer.set_palette(vesper_config::theme::Palette::load()) {
                    bar.redraw();
                }
            }
            TimeoutAction::ToDuration(Duration::from_millis(500))
        })
        .map_err(|e| anyhow!("{e}"))?;

    while !bar.exit {
        if let Err(err) = event_loop.dispatch(None, &mut bar) {
            tracing::info!("compositor connection closed: {err}");
            break;
        }
    }
    Ok(())
}

impl Bar {
    /// Load the enabled plugins' widgets and run the ones that are on the bar (called at start and when the config changes).
    fn sync_plugins(&mut self, config: &vesper_config::Bar, enabled: &[String]) {
        let items = plugin::load(enabled);
        let placed: Vec<plugin::Item> = items
            .iter()
            .filter(|i| [&config.left_items, &config.middle_items, &config.right_items].iter().any(|l| l.iter().any(|x| x == i.id)))
            .cloned()
            .collect();
        self.renderer.plugin_ids = items.iter().map(|i| i.id).collect();
        self.plugin_runner.sync(&placed, &self.dbus_tx);
        self.poller.sys.plugin.retain(|id, _| placed.iter().any(|i| i.id == *id));
        self.plugin_items = items;
    }

    /// The first bar runs one more copy of itself on every other monitor (and stops it when the monitor goes away).
    /// The monitor this bar is on: the name it was started for, or the primary one.
    fn my_output(&self) -> Option<String> {
        self.secondary.clone().or_else(|| self.state.outputs.iter().find(|o| o.primary).map(|o| o.name.clone()))
    }

    fn sync_secondary_bars(&mut self) {
        if self.secondary.is_some() {
            return;
        }
        let want: Vec<String> = if vesper_config::Config::load().bar.all_monitors {
            self.state.outputs.iter().filter(|o| o.enabled && !o.primary).map(|o| o.name.clone()).collect()
        } else {
            Vec::new()
        };
        let stale: Vec<String> = self.children.keys().filter(|n| !want.contains(n)).cloned().collect();
        for n in stale {
            if let Some((mut c, _)) = self.children.remove(&n) {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        self.children.retain(|_, (c, _)| matches!(c.try_wait(), Ok(None)));
        let Ok(exe) = std::env::current_exe() else { return };
        for n in want {
            if self.children.get(&n).is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(3)) {
                continue;
            }
            if self.children.contains_key(&n) {
                continue;
            }
            match std::process::Command::new(&exe).env("VESPER_BAR_OUTPUT", &n).spawn() {
                Ok(c) => {
                    tracing::info!("bar for {n}: started");
                    self.children.insert(n, (c, std::time::Instant::now()));
                }
                Err(e) => tracing::warn!("bar for {n}: could not start: {e}"),
            }
        }
    }

    /// Warn at 20, 10 and 5 percent while discharging, and suspend at 3 if the setting says so.
    fn check_battery(&mut self) {
        if self.secondary.is_some() {
            return; // the first bar does the warnings
        }
        let Some(b) = self.poller.sys.battery else { return };
        if b.charging {
            self.battery_warned = 100;
            self.battery_suspended = false;
            return;
        }
        if b.percent > 25 {
            self.battery_warned = 100;
        }
        for (t, summary) in [(20, "Battery low"), (10, "Battery very low"), (5, "Battery critically low")] {
            if b.percent <= t && self.battery_warned > t {
                self.battery_warned = t;
                self.send(Command::Clipboard { text: String::new(), summary: summary.into(), body: format!("{}% left. Plug in the charger.", b.percent) });
                break;
            }
        }
        if b.percent <= 3 && !self.battery_suspended && vesper_config::Config::load().power.critical_battery_suspend {
            self.battery_suspended = true;
            self.send(Command::Suspend);
        }
    }

    /// Start the media and tray threads once their items are turned on (the tray thread owns the watcher name, so only then).
    fn ensure_dbus(&mut self, items: &[String]) {
        let on = |id: &str| items.iter().any(|i| i == id);
        if on("media") && !self.media_started {
            self.media_started = true;
            dbus::start_media(self.dbus_tx.clone());
        }
        if on("tray") && !self.tray_started && self.secondary.is_none() {
            self.tray_started = true;
            dbus::start_tray(self.dbus_tx.clone());
        }
    }

    /// The span of the bar item under `x`, as its centre.
    fn hit_centre(&self, x: f64) -> i32 {
        let x = x as i32;
        self.hits.iter().find(|(x0, x1, _)| x >= *x0 && x < *x1).map_or(x, |(x0, x1, _)| (x0 + x1) / 2)
    }

    /// Open (or close) the start menu under the system icon; Mod+Escape asks for this.
    fn open_start_menu(&mut self) {
        let x = self.hits.iter().find(|(_, _, h)| *h == Hit::System).map_or(40.0, |(x0, x1, _)| (*x0 + *x1) as f64 / 2.0);
        self.toggle_popup(popup::Kind::Start, x);
    }

    /// Send a command to the compositor after a moment (the start menu has to be off the screen first).
    fn send_later(&mut self, cmd: Command) {
        let Ok(json) = serde_json::to_string(&cmd) else { return };
        if let Ok(mut sock) = self.ipc.try_clone() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                let _ = writeln!(sock, "{json}");
            });
        }
    }

    /// Send a command to the compositor.
    fn send(&mut self, cmd: Command) {
        if let Ok(json) = serde_json::to_string(&cmd) {
            let _ = writeln!(self.ipc, "{json}");
        }
    }

    fn close_popup(&mut self) {
        self.poller.want_stats = false;
        self.poller.sys.stats = None;
        self.popup = None; // dropping the layer surface closes it
        self.popup_hover = None;
        self.popup_drag = None;
    }

    /// Open the popup of `kind` under the bar item at `x` (or close it if that one is already open).
    fn toggle_popup(&mut self, kind: popup::Kind, x: f64) {
        tracing::info!("popup {kind:?} clicked at x={x:.0}");
        if self.popup.as_ref().is_some_and(|p| p.pop.kind == kind) {
            self.close_popup();
            return;
        }
        self.close_popup();
        let anchor = self.hit_centre(x);
        let surface = self.compositor.create_surface(&self.qh);
        let popup_name = match &self.secondary {
            Some(out) => format!("vesper-bar-popup@{out}"),
            None => "vesper-bar-popup".to_string(),
        };
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some(popup_name.as_str()), None);
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_margin(self.height as i32, 0, 0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        tracing::info!("popup {kind:?}: layer surface created");
        layer.commit();
        self.popup = Some(PopupWin { layer, w: 0, h: 0, configured: false, pop: popup::Popup::open(kind, anchor, &self.poller.sys) });
        if kind == popup::Kind::Start {
            // the system numbers are only read while the start menu is open
            self.poller.want_stats = true;
            self.poller.poll();
        }
    }

    fn redraw_popup(&mut self) {
        let Some(pw) = self.popup.as_mut() else { return };
        if !pw.configured || pw.w == 0 || pw.h == 0 {
            return;
        }
        let (w, h) = (pw.w, pw.h);
        let pool = match self.popup_pool.as_mut() {
            Some(p) => p,
            None => match SlotPool::new((w * h * 4) as usize, &self.shm) {
                Ok(p) => self.popup_pool.insert(p),
                Err(_) => return,
            },
        };
        let Ok((buffer, canvas)) = pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        self.renderer.draw_popup(canvas, w, h, &mut pw.pop, &self.poller.sys, self.popup_hover);
        pw.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(pw.layer.wl_surface()).is_ok() {
            pw.layer.commit();
        }
    }

    /// Move the slider `id` to where the pointer is (`x`), and apply it.
    fn set_slider_from(&mut self, id: popup::Slider, x: f64) {
        let Some(pw) = self.popup.as_ref() else { return };
        let Some(((x0, _, x1, _), _)) = pw.pop.hits.iter().find(|(_, h)| *h == popup::PHit::Slider(id)) else { return };
        let frac = ((x - (*x0 + 8) as f64) / ((x1 - x0 - 16).max(1)) as f64).clamp(0.0, 1.0);
        let pct = (frac * 100.0).round() as u32;
        match id {
            popup::Slider::Volume => {
                // the slider is as long as the drawn one: 100%, or 150% with the boost on
                let cur = self.poller.sys.volume.map_or(0, |v| v.percent);
                let vmax = self.renderer.volume_max.max(100).max(cur.min(150));
                let pct = (frac * vmax as f64).round() as u32;
                if self.poller.sys.volume.map(|v| v.percent) == Some(pct) {
                    return;
                }
                popup::spawn("wpctl", &["set-volume", "-l", if vmax > 100 { "1.5" } else { "1.0" }, "@DEFAULT_AUDIO_SINK@", &format!("{pct}%")]);
                if let Some(v) = self.poller.sys.volume.as_mut() {
                    v.percent = pct;
                }
            }
            popup::Slider::Mic => {
                let Some(pw) = self.popup.as_mut() else { return };
                if pw.pop.mic_percent == pct {
                    return;
                }
                pw.pop.mic_percent = pct;
                popup::spawn("wpctl", &["set-volume", "@DEFAULT_AUDIO_SOURCE@", &format!("{pct}%")]);
                self.redraw_popup();
                return;
            }
                    popup::Slider::App(i) => {
                let Some(pw) = self.popup.as_mut() else { return };
                let Some(app) = pw.pop.apps.get_mut(i) else { return };
                if app.percent == pct {
                    return;
                }
                app.percent = pct;
                popup::spawn("wpctl", &["set-volume", &app.id, &format!("{pct}%")]);
                self.redraw_popup();
                return;
            }
            popup::Slider::Night => {
                // kept in the panel while dragging, written to the config when you let go
                let pct = pct.max(1);
                if let Some(pw) = self.popup.as_mut() {
                    pw.pop.night_strength = pct;
                }
                self.redraw_popup();
                return;
            }
            popup::Slider::Brightness => {
                let pct = pct.max(1); // never all the way to a black screen
                if self.poller.sys.brightness.is_none() || self.poller.sys.brightness == Some(pct) {
                    return;
                }
                dbus::set_backlight(pct);
                self.poller.sys.brightness = Some(pct);
            }
        }
        self.redraw_popup();
        self.redraw();
    }

    fn popup_press(&mut self, x: f64, y: f64) {
        let Some(pw) = self.popup.as_mut() else { return };
        let hit = pw.pop.at(x, y);
        if hit.is_none() && !pw.pop.inside(x, y) {
            self.close_popup();
            return;
        }
        match hit {
            Some(popup::PHit::Slider(id)) => {
                self.popup_drag = Some(id);
                self.set_slider_from(id, x);
            }
            Some(popup::PHit::PttLearn) => {
                self.send(Command::LearnPttKey);
                self.close_popup();
                return;
            }
            Some(popup::PHit::Boost) => {
                let on = self.renderer.volume_max <= 100;
                let _ = vesper_config::Config::set("media.volume_boost", on.into());
                self.renderer.volume_max = if on { 150 } else { 100 };
                self.redraw_popup();
                self.redraw();
            }
            Some(popup::PHit::Ptt) => {
                pw.pop.ptt = !pw.pop.ptt;
                let _ = vesper_config::Config::set("media.ptt", pw.pop.ptt.into());
                self.redraw_popup();
            }
            Some(popup::PHit::Open(side)) => {
                if side == popup::Side::Bluetooth {
                    bt::start(self.dbus_tx.clone());
                }
                pw.pop.toggle_side(side);
                self.redraw_popup();
            }
            Some(popup::PHit::Bt(i)) => {
                // connect, disconnect, or pair a new one and connect it
                let st = bt::snapshot();
                if let Some(d) = st.devices.get(i).filter(|d| d.busy.is_none()) {
                    let path = d.path.clone();
                    bt::send(if d.connected { bt::Cmd::Disconnect(path) } else if d.paired { bt::Cmd::Connect(path) } else { bt::Cmd::Pair(path) });
                }
                self.redraw_popup();
            }
            Some(popup::PHit::BtPower) => {
                let on = !bt::snapshot().powered;
                bt::send(bt::Cmd::Power(on));
                if on {
                    bt::send(bt::Cmd::Scan(true));
                }
                self.redraw_popup();
            }
            Some(popup::PHit::BtScan) => {
                bt::send(bt::Cmd::Scan(!bt::snapshot().scanning));
                self.redraw_popup();
            }
            Some(popup::PHit::Source(i)) => {
                if let Some(s) = pw.pop.sources.get(i).cloned() {
                    if pw.pop.ptt {
                        // push to talk: the new microphone starts muted too
                        popup::spawn("sh", &["-c", &format!("wpctl set-default {} && wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 1", s.name)]);
                    } else {
                        popup::spawn("wpctl", &["set-default", &s.name]);
                    }
                    for (j, o) in pw.pop.sources.iter_mut().enumerate() {
                        o.default = j == i;
                    }
                }
                self.redraw_popup();
            }
            Some(popup::PHit::Tile(t)) => {
                let mut q = pw.pop.quick;
                quick::toggle(&mut q, t);
                pw.pop.quick = q;
                if t == quick::Tile::Sound {
                    self.poller.refresh_volume();
                }
                if t == quick::Tile::Mic {
                    self.poller.sys.mic = Some(q.mic); // the bar's microphone item follows at once
                }
                self.redraw_popup();
                self.redraw();
            }
            Some(popup::PHit::Mute) => {
                sys::wpctl(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
                self.poller.refresh_volume();
                pw.pop.quick.sound = self.poller.sys.volume.is_none_or(|v| !v.muted);
                self.redraw_popup();
                self.redraw();
            }
            Some(popup::PHit::Drive(i)) => {
                if let Some(d) = self.poller.sys.drives.get(i).cloned() {
                    drives::open(d);
                }
                self.close_popup();
            }
            Some(popup::PHit::Eject(i)) => {
                if let Some(d) = self.poller.sys.drives.get(i).cloned() {
                    drives::eject(d);
                }
                self.close_popup();
            }
            Some(popup::PHit::Sink(i)) => {
                if let Some(s) = pw.pop.sinks.get(i).cloned() {
                    popup::spawn("wpctl", &["set-default", &s.name]);
                    for (j, o) in pw.pop.sinks.iter_mut().enumerate() {
                        o.default = j == i;
                    }
                    self.redraw_popup();
                }
            }
            Some(popup::PHit::Month(d)) => {
                pw.pop.month_offset += d;
                self.redraw_popup();
            }
            Some(popup::PHit::Profile(i)) => {
                if let Some(name) = pw.pop.profiles.get(i).cloned() {
                    popup::spawn("powerprofilesctl", &["set", &name]);
                    pw.pop.profile = name;
                    self.redraw_popup();
                }
            }
            Some(popup::PHit::Net(i)) => {
                if let Some(n) = pw.pop.nets.get(i).filter(|n| !n.active).cloned() {
                    if n.secure && !popup::is_saved(&n.ssid) {
                        // a new secured network needs its password
                        pw.pop.prompt = Some(popup::Prompt { ssid: n.ssid, ..Default::default() });
                        self.redraw_popup();
                    } else {
                        popup::spawn("nmcli", &["device", "wifi", "connect", &n.ssid]);
                        self.close_popup();
                    }
                }
            }
            Some(popup::PHit::WifiRefresh) => {
                popup::rescan_wifi();
                if let Some(pw) = self.popup.as_mut() {
                    match popup::read_wifi_now() {
                        Ok(n) => {
                            pw.pop.nets = n;
                            pw.pop.nets_error = None;
                        }
                        Err(e) => {
                            pw.pop.nets.clear();
                            pw.pop.nets_error = Some(e);
                        }
                    }
                }
                self.redraw_popup();
            }
            Some(popup::PHit::WifiConnect) => self.wifi_submit(),
            Some(popup::PHit::WifiCancel) => {
                pw.pop.prompt = None;
                self.redraw_popup();
            }
            Some(popup::PHit::Start(act)) => {
                use popup::StartAct as A;
                let power = vesper_config::Config::load().power;
                match act {
                    A::Restart | A::Shutdown if pw.pop.pending != Some(act) => {
                        // a second click confirms
                        pw.pop.pending = Some(act);
                        self.redraw_popup();
                        return;
                    }
                    A::Restart => popup::spawn("sh", &["-c", &power.reboot()]),
                    A::Shutdown => popup::spawn("sh", &["-c", &power.poweroff()]),
                    A::Settings => self.send(Command::Settings { page: None }),
                    A::Wallpapers => self.send(Command::Wallpapers),
                    // the menu has to be gone from the frozen picture, so wait for it to disappear
                    A::Screenshot => self.send_later(Command::Screenshot { kind: "freeze".into(), x: None, y: None, w: None, h: None }),
                    A::CopyText => self.send_later(Command::Screenshot { kind: "freeze-text".into(), x: None, y: None, w: None, h: None }),
                    A::ReadQr => self.send_later(Command::Screenshot { kind: "freeze-qr".into(), x: None, y: None, w: None, h: None }),
                    A::Record => {
                        popup::spawn(&popup::recorder(), &[]);
                        self.close_popup();
                    }
                    A::Lock => self.send(Command::Lock),
                    A::Sleep => self.send(Command::Suspend),
                    A::Logout => self.send(Command::Logout),
                    A::Apps => self.send(Command::Launcher),
                    A::PowerMenu => self.send(Command::PowerMenu),
                    A::Files => self.send(Command::Files),
                    A::Clipboard => self.send(Command::LauncherMode { mode: "clipboard".into() }),
                    A::Emoji => self.send(Command::LauncherMode { mode: "emoji".into() }),
                }
                self.close_popup();
            }
            None => {}
        }
    }

    fn redraw(&mut self) {
        if !self.configured || self.width == 0 {
            return;
        }
        let (w, h) = (self.width, self.height);
        let stride = w as i32 * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888) else {
            tracing::warn!("could not allocate a {w}x{h} buffer");
            return;
        };
        let (hits, shapes) = self.renderer.draw(canvas, w, h, &self.state, &self.clock, &self.poller.sys, self.hover);
        self.hits = hits;
        // tell the compositor what is drawn, so blur follows the real shape
        let rects: Vec<[i32; 4]> = shapes.iter().map(|s| s.body()).collect();
        if rects != self.last_shapes {
            if self.secondary.is_some() {
                // only the first bar tells the compositor its shape (for blur)
            } else if let Ok(json) = serde_json::to_string(&Command::BarShapes { rects: rects.clone() }) {
                let _ = writeln!(self.ipc, "{json}");
            }
            self.last_shapes = rects;
        }
        self.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(self.layer.wl_surface()).is_ok() {
            self.layer.commit();
        }
        let _ = &self.qh;
    }

    /// Split the bar after icon `id`, or join it again if it is already split here.
    fn toggle_split(&mut self, id: &str) {
        let mut splits = self.renderer.splits.clone();
        match splits.iter().position(|s| s == id) {
            Some(i) => {
                splits.remove(i);
            }
            None => splits.push(id.to_owned()),
        }
        let value = toml::Value::Array(splits.iter().cloned().map(toml::Value::String).collect());
        let _ = vesper_config::Config::set("bar.splits", value);
        self.renderer.splits = splits;
        self.config_stamp = vesper_config::Config::stamp();
        self.redraw();
    }

    fn hit_at(&self, x: f64) -> Option<Hit> {
        let x = x as i32;
        self.hits.iter().find(|(x0, x1, _)| x >= *x0 && x < *x1).map(|(_, _, h)| *h)
    }

    fn click(&mut self, x: f64) {
        let hit = self.hit_at(x);
        // a click on the bar closes an open popup (unless it opens one)
        if !matches!(hit, Some(Hit::Clock | Hit::System | Hit::Battery | Hit::Network | Hit::Drives | Hit::Volume | Hit::Brightness)) {
            self.close_popup();
        }
        let cmd = match hit {
            Some(Hit::Clock) => {
                self.toggle_popup(popup::Kind::Calendar, x);
                return;
            }
            Some(Hit::Battery) => {
                self.toggle_popup(popup::Kind::Battery, x);
                return;
            }
            Some(Hit::Network) => {
                self.toggle_popup(popup::Kind::Network, x);
                return;
            }
            Some(Hit::Drives) => {
                self.toggle_popup(popup::Kind::Drives, x);
                return;
            }
            Some(Hit::Workspace(index)) => Command::Workspace { index, output: self.my_output() },
            Some(Hit::Restore(index)) => Command::Restore { index, output: self.my_output() },
            Some(Hit::Gap(id)) => {
                self.toggle_split(id);
                return;
            }
            Some(Hit::System) => {
                self.poller.sys.brightness = sys::read_brightness();
                self.toggle_popup(popup::Kind::Start, x);
                return;
            }
            Some(Hit::Bell) => Command::NotificationCenter,
            Some(Hit::Keyboard) => Command::CycleLayout,
            Some(Hit::Plugin(id)) => {
                if let Some(item) = self.plugin_items.iter().find(|i| i.id == id) {
                    plugin::click(item);
                }
                return;
            }
            Some(Hit::Mic) => {
                sys::wpctl(&["set-mute", "@DEFAULT_AUDIO_SOURCE@", "1"]);
                self.poller.sys.mic = Some(false);
                self.redraw();
                return;
            }
            Some(Hit::Brightness) => {
                self.poller.sys.brightness = sys::read_brightness();
                self.toggle_popup(popup::Kind::Brightness, x);
                return;
            }
            Some(Hit::MediaPrev | Hit::MediaPlay | Hit::MediaNext) => {
                if let Some(m) = self.poller.sys.media.clone() {
                    let method = match hit { Some(Hit::MediaPrev) => "Previous", Some(Hit::MediaNext) => "Next", _ => "PlayPause" };
                    dbus::media_call(m.player, method);
                }
                return;
            }
            Some(Hit::Tray(i)) => {
                if let Some(t) = self.poller.sys.tray.get(i) {
                    dbus::tray_call(t, "Activate", x as i32, self.height as i32);
                }
                return;
            }
            Some(Hit::Volume) => {
                self.toggle_popup(popup::Kind::Volume, x);
                return;
            }
            None => return,
        };
        if let Ok(json) = serde_json::to_string(&cmd) {
            let _ = writeln!(self.ipc, "{json}");
        }
    }
}

impl LayerShellHandler for Bar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.osd.as_ref().is_some_and(|o| o.layer.wl_surface() == layer.wl_surface()) {
            self.osd = None;
            return;
        }
        if self.popup.as_ref().is_some_and(|p| p.layer.wl_surface() == layer.wl_surface()) {
            self.close_popup();
            tracing::warn!("the compositor closed the popup");
            return;
        }
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let Some(o) = self.osd.as_mut().filter(|o| o.layer.wl_surface() == layer.wl_surface()) {
            o.configured = true;
            self.redraw_osd();
            return;
        }
        if let Some(pw) = self.popup.as_mut().filter(|p| p.layer.wl_surface() == layer.wl_surface()) {
            pw.w = c.new_size.0;
            pw.h = c.new_size.1;
            tracing::info!("popup configured {}x{}", c.new_size.0, c.new_size.1);
            pw.configured = true;
            self.popup_pool = None; // sized for the old screen
            self.redraw_popup();
            return;
        }
        self.width = NonZeroU32::new(c.new_size.0).map_or(1280, NonZeroU32::get);
        if let Some(h) = NonZeroU32::new(c.new_size.1) {
            self.height = h.get();
        }
        self.configured = true;
        self.clock = chrono::Local::now().format("%H:%M\u{1}%a %d").to_string();
        self.redraw();
    }
}

impl PointerHandler for Bar {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if self.popup.as_ref().is_some_and(|p| &e.surface == p.layer.wl_surface()) {
                match e.kind {
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                        if let Some(id) = self.popup_drag {
                            self.set_slider_from(id, e.position.0);
                        }
                        let hover = self.popup.as_ref().and_then(|p| p.pop.at(e.position.0, e.position.1));
                        if hover != self.popup_hover {
                            self.popup_hover = hover;
                            self.redraw_popup();
                        }
                    }
                    PointerEventKind::Leave { .. } => {
                        if self.popup_hover.take().is_some() {
                            self.redraw_popup();
                        }
                    }
                    PointerEventKind::Press { button: BTN_LEFT, .. } => self.popup_press(e.position.0, e.position.1),
                    PointerEventKind::Release { .. } => {
                        if self.popup_drag == Some(popup::Slider::Night) {
                            if let Some(pw) = self.popup.as_ref() {
                                let _ = vesper_config::Config::set("appearance.night_light_strength", (pw.pop.night_strength as i64).into());
                            }
                        }
                        self.popup_drag = None;
                    }
                    // right-click a switch that has a side panel to open it
                    PointerEventKind::Press { button: BTN_RIGHT, .. } => {
                        let hit = self.popup.as_ref().and_then(|p| p.pop.at(e.position.0, e.position.1));
                        // right-click a Bluetooth device: forget it
                        if let Some(popup::PHit::Bt(i)) = hit {
                            if let Some(d) = bt::snapshot().devices.get(i).filter(|d| d.paired) {
                                bt::send(bt::Cmd::Forget(d.path.clone()));
                            }
                            continue;
                        }
                        if let (Some(popup::PHit::Tile(t)), Some(pw)) = (hit, self.popup.as_mut()) {
                            if let Some(side) = popup::side_for(t) {
                                if side == popup::Side::Bluetooth {
                                    bt::start(self.dbus_tx.clone());
                                }
                                pw.pop.toggle_side(side);
                                self.redraw_popup();
                            }
                        }
                    }
                    PointerEventKind::Axis { vertical, .. } if self.popup.as_ref().is_some_and(|p| p.pop.kind == popup::Kind::Volume) => {
                        let up = vertical.absolute < 0.0 || vertical.discrete < 0;
                        let down = vertical.absolute > 0.0 || vertical.discrete > 0;
                        if up || down {
                            sys::wpctl(&["set-volume", "-l", if self.renderer.volume_max > 100 { "1.5" } else { "1.0" }, "@DEFAULT_AUDIO_SINK@", if up { "5%+" } else { "5%-" }]);
                            self.poller.refresh_volume();
                            self.redraw_popup();
                            self.redraw();
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let hover = self.hit_at(e.position.0);
                    if hover != self.hover {
                        self.hover = hover;
                        self.redraw();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if self.hover.take().is_some() {
                        self.redraw();
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => self.click(e.position.0),
                PointerEventKind::Press { button: BTN_RIGHT, .. } if matches!(self.hit_at(e.position.0), Some(Hit::Tray(_))) => {
                    if let Some(Hit::Tray(i)) = self.hit_at(e.position.0) {
                        if let Some(t) = self.poller.sys.tray.get(i) {
                            dbus::tray_call(t, "ContextMenu", e.position.0 as i32, self.height as i32);
                        }
                    }
                }
                PointerEventKind::Press { button: BTN_RIGHT, .. } if self.hit_at(e.position.0) == Some(Hit::Volume) => {
                    sys::wpctl(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
                    self.poller.refresh_volume();
                    self.redraw();
                }
                PointerEventKind::Press { button: BTN_RIGHT, .. } if self.hit_at(e.position.0) == Some(Hit::Bell) => {
                    // right-click the bell: do not disturb on / off
                    let on = !vesper_config::Config::load().notifications.do_not_disturb;
                    let _ = vesper_config::Config::set("notifications.do_not_disturb", on.into());
                    self.poller.sys.notif.dnd = on;
                    self.redraw();
                }
                PointerEventKind::Axis { vertical, .. } if self.hit_at(e.position.0) == Some(Hit::Brightness) => {
                    if vertical.absolute != 0.0 || vertical.discrete != 0 {
                        let up = vertical.absolute < 0.0 || vertical.discrete < 0;
                        if let Some(cur) = self.poller.sys.brightness {
                            let next = (cur as i32 + if up { 5 } else { -5 }).clamp(1, 100) as u32;
                            dbus::set_backlight(next);
                            self.poller.sys.brightness = Some(next);
                        }
                        self.redraw();
                        self.show_osd("brightness", false);
                    }
                }
                PointerEventKind::Axis { vertical, .. } if self.hit_at(e.position.0) == Some(Hit::Volume) => {
                    let up = vertical.absolute < 0.0 || vertical.discrete < 0;
                    let down = vertical.absolute > 0.0 || vertical.discrete > 0;
                    if up || down {
                        sys::wpctl(&["set-volume", "-l", if self.renderer.volume_max > 100 { "1.5" } else { "1.0" }, "@DEFAULT_AUDIO_SINK@", if up { "5%+" } else { "5%-" }]);
                        self.poller.refresh_volume();
                        self.redraw();
                        self.show_osd("volume", false);
                    }
                }
                _ => {}
            }
        }
    }
}

impl SeatHandler for Bar {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let handle = KEY_LOOP.with(|k| k.borrow().clone());
            self.keyboard = match handle {
                Some(h) => self.seat_state.get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|bar: &mut Bar, _, ev| bar.popup_key(ev))).ok(),
                None => self.seat_state.get_keyboard(qh, &seat, None).ok(),
            };
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for Bar {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Bar {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Bar);
delegate_output!(Bar);
delegate_shm!(Bar);
delegate_seat!(Bar);
delegate_pointer!(Bar);
delegate_keyboard!(Bar);
delegate_layer!(Bar);
delegate_registry!(Bar);

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

thread_local! {
    static KEY_LOOP: std::cell::RefCell<Option<smithay_client_toolkit::reexports::calloop::LoopHandle<'static, Bar>>> =
        const { std::cell::RefCell::new(None) };
}

impl Bar {
    /// A key pressed while a popup has the keyboard: Esc closes it (or the password prompt), and typing fills the prompt.
    fn popup_key(&mut self, event: KeyEvent) {
        let Some(pw) = self.popup.as_mut() else { return };
        if event.keysym == Keysym::Escape {
            if pw.pop.prompt.take().is_some() {
                self.redraw_popup();
            } else {
                self.close_popup();
            }
            return;
        }
        let Some(pr) = pw.pop.prompt.as_mut() else { return };
        if pr.busy {
            return;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => {
                self.wifi_submit();
                return;
            }
            Keysym::BackSpace => {
                pr.text.pop();
                pr.error = None;
            }
            _ => {
                if let Some(s) = event.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        pr.text.push_str(&printable);
                        pr.error = None;
                    }
                }
            }
        }
        self.redraw_popup();
    }

    /// Try the password that was typed: `nmcli` joins the network on its own thread and reports back.
    fn wifi_submit(&mut self) {
        let Some(pr) = self.popup.as_mut().and_then(|p| p.pop.prompt.as_mut()) else { return };
        if pr.busy || pr.text.is_empty() {
            return;
        }
        pr.busy = true;
        pr.error = None;
        let (ssid, pass) = (pr.ssid.clone(), pr.text.clone());
        let tx = self.dbus_tx.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("nmcli").args(["device", "wifi", "connect", &ssid, "password", &pass]).output();
            let res = match out {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => {
                    // a failed attempt leaves a useless saved connection behind
                    let _ = std::process::Command::new("nmcli").args(["connection", "delete", "id", &ssid]).output();
                    let msg = String::from_utf8_lossy(&o.stderr).to_string();
                    Err(if msg.contains("Secrets were required") || msg.contains("802-11") || msg.contains("Connection activation failed") {
                        "Wrong password, or could not join".to_string()
                    } else {
                        msg.lines().next().unwrap_or("Could not connect").to_string()
                    })
                }
                Err(e) => Err(format!("nmcli is not installed ({e})")),
            };
            let _ = tx.send(dbus::Dbus::Wifi(res));
        });
        self.redraw_popup();
    }
}

impl KeyboardHandler for Bar {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        // the keyboard went elsewhere (a window was clicked, another VT): a popup is not kept open behind your back
        if self.popup.as_ref().is_some_and(|p| p.configured) {
            self.close_popup();
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.popup_key(event);
    }

    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.popup_key(event);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

/// The on-screen popup shown for a moment when the volume or brightness changes.
struct OsdWin {
    layer: LayerSurface,
    w: u32,
    h: u32,
    configured: bool,
    kind: &'static str,
    hide_at: std::time::Instant,
}

impl Bar {
    /// Show (or refresh) the volume or brightness popup for about a second and a half. `refresh` reads the value anew
    /// (for changes made by the keys); scrolling in the bar has already updated it.
    fn show_osd(&mut self, kind: &'static str, refresh: bool) {
        use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
        if self.popup.is_some() || self.secondary.is_some() || !vesper_config::Config::load().bar.osd {
            return; // the menu is on screen showing the same thing, or the popup is switched off
        }
        if refresh {
            if kind == "volume" {
                self.poller.refresh_volume();
            } else {
                self.poller.sys.brightness = sys::read_brightness();
            }
        }
        let hide_at = std::time::Instant::now() + Duration::from_millis(1500);
        if let Some(o) = self.osd.as_mut() {
            o.kind = kind;
            o.hide_at = hide_at;
            self.redraw_osd();
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        // the popup never takes clicks: an empty input region lets them through to what is underneath
        if let Ok(region) = smithay_client_toolkit::compositor::Region::new(&self.compositor) {
            surface.set_input_region(Some(region.wl_region()));
        }
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("vesper-osd"), None);
        let (w, h) = (300u32, 64u32);
        layer.set_anchor(Anchor::TOP);
        layer.set_margin(self.height as i32 + 12, 0, 0, 0);
        layer.set_size(w, h);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        self.osd = Some(OsdWin { layer, w, h, configured: false, kind, hide_at });
        if let Some(handle) = KEY_LOOP.with(|k| k.borrow().clone()) {
            let _ = handle.insert_source(Timer::from_duration(Duration::from_millis(1500)), |_, _, bar: &mut Bar| match bar.osd.as_ref() {
                Some(o) if std::time::Instant::now() < o.hide_at => TimeoutAction::ToDuration(o.hide_at - std::time::Instant::now()),
                _ => {
                    bar.osd = None;
                    TimeoutAction::Drop
                }
            });
        }
    }

    fn redraw_osd(&mut self) {
        let Some(o) = self.osd.as_ref() else { return };
        if !o.configured || o.w == 0 || o.h == 0 {
            return;
        }
        let (w, h, kind) = (o.w, o.h, o.kind);
        let (pct, muted) = if kind == "volume" {
            self.poller.sys.volume.map_or((0, false), |v| (v.percent, v.muted))
        } else {
            (self.poller.sys.brightness.unwrap_or(0), false)
        };
        let pool = match self.osd_pool.as_mut() {
            Some(p) => p,
            None => match SlotPool::new((w * h * 4) as usize, &self.shm) {
                Ok(p) => self.osd_pool.insert(p),
                Err(_) => return,
            },
        };
        let Ok((buffer, canvas)) = pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        self.renderer.draw_osd(canvas, w, h, kind, pct, muted);
        let Some(o) = self.osd.as_ref() else { return };
        o.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(o.layer.wl_surface()).is_ok() {
            o.layer.commit();
        }
    }
}

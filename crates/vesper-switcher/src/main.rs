//! vesper-switcher: a centred overlay listing every window. Mod+Tab opens it with the next window selected, Tab and
//! Shift+Tab (or the arrows) move, releasing the shortcut key or pressing Enter switches, Esc cancels. Clicking a row works too.
//! Styled after the top bar like the launcher.

use std::{io::{BufRead, BufReader, Write}, num::NonZeroU32, os::unix::net::UnixStream, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::client::{
        globals::registry_queue_init,
        protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        Connection, QueueHandle,
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
use vesper_apps::{Apps, Icons};
use vesper_config::theme::{alpha, with_alpha, Palette};
use vesper_gfx::{Canvas, Image, Rgba, Text};
use vesper_ipc::{Command, State};

const ROW_H: i32 = 46;
const MAX_ROWS: usize = 9;
const PAD: i32 = 14;
const ICON: i32 = 28;
const WIDTH: u32 = 560;
const BTN_LEFT: u32 = 0x110;
const TILE_TEXT: Rgba = [0xff, 0xff, 0xff, 0xf0];

struct Entry {
    workspace: usize,
    index: usize,
    app_id: String,
    title: String,
}

struct Switcher {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    width: u32,
    height: u32,
    configured: bool,
    exit: bool,

    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    /// The shortcut modifier (Super or Alt) was down: letting go of it picks the window.
    mod_was_down: bool,
    chosen: Option<usize>,

    apps: Apps,
    icons: Icons,
    text: Text,
    small: Text,
    pal: Palette,
    radius: f32,
    border_w: f32,
    border: Option<Rgba>,
    bg_alpha: u8,
    row_rects: Vec<(i32, i32, i32, i32, usize)>,
}

fn read_state() -> Result<State> {
    let path = vesper_ipc::socket_path();
    let sock = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(sock).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();
    let state = read_state().context("asking the compositor for its windows")?;
    let mut entries = Vec::new();
    for (wi, ws) in state.workspaces.iter().enumerate() {
        for (i, w) in ws.windows.iter().enumerate() {
            entries.push(Entry { workspace: wi, index: i, app_id: w.app_id.clone(), title: w.title.clone() });
        }
    }
    if entries.is_empty() {
        return Ok(());
    }
    // start on the window after the focused one
    let focused = state.focused.as_ref().and_then(|f| entries.iter().position(|e| e.app_id == f.app_id && e.title == f.title));
    let selected = match focused {
        Some(i) => (i + 1) % entries.len(),
        None => 0,
    };

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;
    let rows = entries.len().min(MAX_ROWS) as i32;
    let height = (2 * PAD + rows * ROW_H) as u32;
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-switcher"), None);
    layer.set_anchor(Anchor::empty());
    layer.set_size(WIDTH, height);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let bar = &config.bar;
    let text = Text::new(&bar.font, 16.0);
    let mut app = Switcher {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(WIDTH as usize * 700 * 4, &shm)?,
        shm,
        layer,
        keyboard: None,
        pointer: None,
        width: WIDTH,
        height,
        configured: false,
        exit: false,
        entries,
        selected,
        scroll: 0,
        mod_was_down: false,
        chosen: None,
        apps: Apps::load(),
        icons: Icons::default(),
        small: text.resized(12.0),
        text,
        pal: Palette::load(),
        radius: bar.radius.clamp(6, 30) as f32,
        border_w: bar.border_width.clamp(0, 6).max(1) as f32,
        border: vesper_config::theme::hex(&bar.border_color),
        bg_alpha: (bar.opacity.clamp(20, 100) * 255 / 100) as u8,
        row_rects: Vec::new(),
    };
    app.ensure_visible();
    while !app.exit {
        queue.blocking_dispatch(&mut app)?;
    }
    // take the overlay away first, then switch
    let chosen = app.chosen;
    let entry = chosen.and_then(|i| app.entries.get(i)).map(|e| (e.workspace, e.index));
    drop(app); // takes the overlay away
    let _ = conn.roundtrip();
    drop(queue);
    if let Some((workspace, index)) = entry {
        let path = vesper_ipc::socket_path();
        let mut sock = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
        writeln!(sock, "{}", serde_json::to_string(&Command::FocusWindow { workspace, index })?)?;
        sock.flush()?;
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

impl Switcher {
    fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + MAX_ROWS {
            self.scroll = self.selected + 1 - MAX_ROWS;
        }
    }

    fn step(&mut self, d: isize) {
        let n = self.entries.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + d).rem_euclid(n) as usize;
            self.ensure_visible();
            self.redraw();
        }
    }

    fn confirm(&mut self) {
        self.chosen = Some(self.selected);
        self.exit = true;
    }

    fn redraw(&mut self) {
        if !self.configured {
            return;
        }
        let (w, h) = (self.width, self.height);
        let rows = self.entries.len().saturating_sub(self.scroll).min(MAX_ROWS);
        let icons: Vec<Option<Image>> = (0..rows)
            .map(|r| {
                let e = &self.entries[self.scroll + r];
                let app = self.apps.find(&e.app_id);
                let name = app.map(|a| a.icon.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| e.app_id.clone());
                self.icons.get(&name, ICON)
            })
            .collect();
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let p = &self.pal;
        let accent = p.primary;
        c.rounded_rect(0, 0, w as i32, h as i32, self.radius, alpha(p.surface, self.bg_alpha));
        c.rounded_outline(0, 0, w as i32, h as i32, self.radius, self.border_w, self.border.unwrap_or(accent));
        self.row_rects.clear();
        for r in 0..rows {
            let idx = self.scroll + r;
            let e = &self.entries[idx];
            let y = PAD + r as i32 * ROW_H;
            let rr = (PAD, y, w as i32 - 2 * PAD, ROW_H - 2);
            if idx == self.selected {
                c.rounded_rect(rr.0, rr.1, rr.2, rr.3, (self.radius - 4.0).max(8.0), with_alpha(accent, 0.30));
            }
            self.row_rects.push((rr.0, rr.1, rr.2, rr.3, idx));
            let iy = y + (ROW_H - 2 - ICON) / 2;
            match &icons[r] {
                Some(img) => c.image(PAD + 12, iy, img),
                None => {
                    c.rounded_rect(PAD + 12, iy, ICON, ICON, ICON as f32 * 0.24, [0x55, 0x5a, 0x6a, 0xff]);
                    let initial: String = e.app_id.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                    self.small.draw_centered(&mut c, PAD + 12 + ICON / 2, iy + ICON / 2, &initial, TILE_TEXT);
                }
            }
            let ws = format!("Workspace {}", e.workspace + 1);
            let tx = PAD + 12 + ICON + 12;
            let avail = w as i32 - tx - PAD - self.small.width(&ws) - 24;
            let title = if e.title.is_empty() { e.app_id.clone() } else { e.title.clone() };
            self.text.draw(&mut c, tx, y + (ROW_H - 2) / 2, &self.text.fit(&title, avail), p.on_surface);
            self.small.draw(&mut c, w as i32 - PAD - 12 - self.small.width(&ws), y + (ROW_H - 2) / 2, &ws, p.on_surface_variant);
        }
        self.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(self.layer.wl_surface()).is_ok() {
            self.layer.commit();
        }
    }
}

impl LayerShellHandler for Switcher {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        self.width = NonZeroU32::new(c.new_size.0).map_or(self.width, NonZeroU32::get);
        self.height = NonZeroU32::new(c.new_size.1).map_or(self.height, NonZeroU32::get);
        self.configured = true;
        self.redraw();
    }
}

impl KeyboardHandler for Switcher {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        if self.configured {
            self.exit = true; // focus went elsewhere: cancel
        }
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Return | Keysym::KP_Enter => self.confirm(),
            Keysym::Tab | Keysym::Down | Keysym::Right => self.step(1),
            Keysym::ISO_Left_Tab | Keysym::Up | Keysym::Left => self.step(-1),
            _ => {}
        }
    }
    fn repeat_key(&mut self, c: &Connection, q: &QueueHandle<Self>, k: &wl_keyboard::WlKeyboard, s: u32, event: KeyEvent) {
        self.press_key(c, q, k, s, event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        // Mod+Tab opened us: letting go of Super/Alt picks the highlighted window
        if m.logo || m.alt {
            self.mod_was_down = true;
        } else if self.mod_was_down {
            self.confirm();
        }
    }
}

impl PointerHandler for Switcher {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            let hit = |s: &Self| s.row_rects.iter().find(|(x, y, w, h, _)| e.position.0 as i32 >= *x && (e.position.0 as i32) < x + w && e.position.1 as i32 >= *y && (e.position.1 as i32) < y + h).map(|r| r.4);
            match e.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    if let Some(i) = hit(self).filter(|i| *i != self.selected) {
                        self.selected = i;
                        self.redraw();
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    if let Some(i) = hit(self) {
                        self.selected = i;
                        self.confirm();
                    }
                }
                _ => {}
            }
        }
    }
}

impl SeatHandler for Switcher {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        match cap {
            Capability::Keyboard if self.keyboard.is_none() => self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok(),
            Capability::Pointer if self.pointer.is_none() => self.pointer = self.seat_state.get_pointer(qh, &seat).ok(),
            _ => {}
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        match cap {
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
            }
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            _ => {}
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for Switcher {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Switcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Switcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Switcher);
delegate_output!(Switcher);
delegate_shm!(Switcher);
delegate_seat!(Switcher);
delegate_keyboard!(Switcher);
delegate_pointer!(Switcher);
delegate_layer!(Switcher);
delegate_registry!(Switcher);

impl ProvidesRegistryState for Switcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

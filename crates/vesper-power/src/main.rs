//! vesper-power: a centred row of power actions over a dimmed screen.
//!
//! Left/Right or Tab to choose, Enter to run, Esc (or a click outside) to close.

use std::{io::Write, num::NonZeroU32, os::unix::net::UnixStream};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
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
use vesper_config::theme::{alpha, with_alpha, Palette};
use vesper_gfx::{Canvas, Text};

const BTN_LEFT: u32 = 0x110;

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Lock,
    Sleep,
    Logout,
    Restart,
    Shutdown,
}

const CHOICES: [(Choice, &str, &str); 5] = [
    (Choice::Lock, "\u{f033e}", "Lock"),
    (Choice::Sleep, "\u{f04b2}", "Sleep"),
    (Choice::Logout, "\u{f0343}", "Log out"),
    (Choice::Restart, "\u{f0709}", "Restart"),
    (Choice::Shutdown, "\u{f0425}", "Shut down"),
];

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    size: (u32, u32),
    configured: bool,
    exit: bool,
    sel: usize,
    pal: Palette,
    text: Text,
    big: Text,
    config: vesper_config::Config,
    cells: Vec<(i32, i32, i32, i32)>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let config = vesper_config::Config::load();
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-power"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_size(0, 0);
    layer.commit();

    let text = Text::new(&config.bar.font, 16.0);
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(1920 * 1080 * 4, &shm)?,
        shm,
        layer,
        keyboard: None,
        pointer: None,
        size: (0, 0),
        configured: false,
        exit: false,
        sel: 0,
        pal: Palette::load(),
        big: text.resized(34.0),
        text,
        config,
        cells: Vec::new(),
    };
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(conn, event_queue).insert(event_loop.handle()).map_err(|e| anyhow!("{e}"))?;
    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

impl App {
    fn run(&mut self, c: Choice) {
        let send = |cmd: vesper_ipc::Command| {
            if let Ok(mut s) = UnixStream::connect(vesper_ipc::socket_path()) {
                let _ = writeln!(s, "{}", serde_json::to_string(&cmd).unwrap_or_default());
                let _ = s.flush();
                // stay connected a moment so the compositor reads the command before the line closes
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        };
        let shell = |cmd: String| {
            let _ = std::process::Command::new("sh").arg("-c").arg(cmd).spawn();
        };
        match c {
            Choice::Lock => send(vesper_ipc::Command::Lock),
            Choice::Sleep => send(vesper_ipc::Command::Suspend),
            Choice::Logout => send(vesper_ipc::Command::Logout),
            Choice::Restart => shell(self.config.power.reboot()),
            Choice::Shutdown => shell(self.config.power.poweroff()),
        }
        self.exit = true;
    }

    fn draw(&mut self) {
        if !self.configured || self.size.0 == 0 {
            return;
        }
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas::new(canvas, w as u32, h as u32);
        // like the start menu and the other popouts: no dimmed screen, just the panel
        c.clear([0, 0, 0, 0]);
        let cell = 120;
        let gap = 14;
        let n = CHOICES.len() as i32;
        let total = n * cell + (n - 1) * gap;
        let (x0, y0) = ((w - total) / 2, (h - cell) / 2);
        let b = &self.config.bar;
        let fill = (b.opacity.clamp(40, 100) * 255 / 100) as u8;
        let mut border = vesper_config::theme::hex(&b.border_color).unwrap_or(self.pal.primary);
        if b.border_blend {
            border[3] = (border[3] as u32 * fill as u32 / 255) as u8;
        }
        let radius = b.radius.clamp(8, 28) as f32;
        c.rounded_rect(x0 - 24, y0 - 24, total + 48, cell + 48, radius, alpha(self.pal.surface, fill));
        c.rounded_outline(x0 - 24, y0 - 24, total + 48, cell + 48, radius, b.border_width.clamp(0, 6).max(1) as f32, border);
        self.cells.clear();
        for (i, (_, icon, label)) in CHOICES.iter().enumerate() {
            let x = x0 + i as i32 * (cell + gap);
            let selected = i == self.sel;
            let r = (x, y0, cell, cell);
            let danger = matches!(CHOICES[i].0, Choice::Shutdown | Choice::Restart);
            let bg = if selected { if danger { self.pal.error } else { self.pal.primary } } else { with_alpha(self.pal.on_surface, 0.08) };
            let fg = if selected { if danger { self.pal.surface } else { self.pal.on_primary } } else { self.pal.on_surface };
            c.rounded_rect(r.0, r.1, r.2, r.3, 12.0, bg);
            self.big.draw_centered(&mut c, x + cell / 2, y0 + cell / 2 - 10, icon, fg);
            self.text.draw_centered(&mut c, x + cell / 2, y0 + cell - 26, label, fg);
            self.cells.push(r);
        }
        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(c.new_size.0), NonZeroU32::new(c.new_size.1)) {
            self.size = (w.get(), h.get());
        }
        self.configured = true;
        self.draw();
    }
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        let n = CHOICES.len();
        match e.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::Left | Keysym::h => self.sel = (self.sel + n - 1) % n,
            Keysym::Right | Keysym::l | Keysym::Tab => self.sel = (self.sel + 1) % n,
            Keysym::Return | Keysym::KP_Enter => self.run(CHOICES[self.sel].0),
            _ => return,
        }
        self.draw();
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            let (x, y) = (e.position.0 as i32, e.position.1 as i32);
            let over = self.cells.iter().position(|r| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3);
            match e.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    if let Some(i) = over.filter(|i| *i != self.sel) {
                        self.sel = i;
                        self.draw();
                    }
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => match over {
                    Some(i) => self.run(CHOICES[i].0),
                    None => self.exit = true,
                },
                _ => {}
            }
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
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
delegate_layer!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

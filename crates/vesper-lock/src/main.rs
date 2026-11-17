//! vesper-lock: the lock screen. Locks the session with ext-session-lock, shows the time and a password field,
//! and unlocks when PAM accepts the password. The compositor keeps the session locked if this process dies.

mod pam;
mod switch;

use std::{cell::RefCell, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel,
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
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
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_config::theme::Palette;
use vesper_gfx::{Canvas, Text};


#[derive(Clone, Copy, PartialEq)]
enum Status {
    Idle,
    Checking,
    Wrong,
}

struct Surf {
    surface: SessionLockSurface,
    size: (u32, u32),
}

struct Lock {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    pool: SlotPool,
    conn: Connection,
    qh: QueueHandle<Lock>,
    session_lock: Option<SessionLock>,
    surfaces: Vec<Surf>,
    /// Outputs that already have a lock surface (the protocol allows only one each).
    covered: Vec<wl_output::WlOutput>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// Command for the "Switch user" button, if this system has a display manager to switch to.
    switch_cmd: Option<String>,
    hover_switch: bool,
    auth_tx: channel::Sender<bool>,

    user: String,
    password: String,
    status: Status,
    big: Text,
    medium: Text,
    pal: Palette,
    small: Text,
    exit: bool,
    exit_code: i32,
}

thread_local! {
    static LOOP: RefCell<Option<LoopHandle<'static, Lock>>> = const { RefCell::new(None) };
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("vesper-lock: {err:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    let config = vesper_config::Config::load();
    let user = pam::current_user().ok_or_else(|| anyhow!("could not determine the current user"))?;

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;
    let lock_state = SessionLockState::new(&globals, &qh);

    let (auth_tx, auth_rx) = channel::channel::<bool>();
    let text = Text::new(&config.bar.font, 18.0);

    let mut app = Lock {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        pool: SlotPool::new(1920 * 1080 * 4, &shm)?,
        shm,
        conn: conn.clone(),
        qh: qh.clone(),
        session_lock: None,
        surfaces: Vec::new(),
        covered: Vec::new(),
        keyboard: None,
        pointer: None,
        switch_cmd: switch::system_probe_resolve(&config.lock.switch_user),
        hover_switch: false,
        auth_tx,
        user,
        password: String::new(),
        status: Status::Idle,
        big: text.resized(120.0),
        medium: text.resized(26.0),
        pal: Palette::load(),
        small: text.resized(17.0),
        exit: false,
        exit_code: 0,
    };

    // Ask for the lock. If the compositor refuses, we get `finished` and exit.
    app.session_lock = Some(lock_state.lock(&qh).map_err(|e| anyhow!("this compositor does not support ext-session-lock: {e}"))?);
    for output in app.output_state.outputs() {
        app.add_surface(&output);
    }

    let mut event_loop: EventLoop<Lock> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    LOOP.with(|l| *l.borrow_mut() = Some(handle.clone()));
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(auth_rx, |event, _, app| {
            if let channel::Event::Msg(ok) = event {
                app.auth_done(ok);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    // Keep the clock current.
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, app| {
            app.pal = Palette::load();
            app.redraw_all();
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow!("{e}"))?;

    while !app.exit {
        event_loop.dispatch(Duration::from_millis(100), &mut app)?;
    }
    Ok(app.exit_code)
}

impl Lock {
    fn add_surface(&mut self, output: &wl_output::WlOutput) {
        let Some(lock) = &self.session_lock else { return };
        if self.covered.contains(output) {
            return;
        }
        self.covered.push(output.clone());
        let surface = self.compositor.create_surface(&self.qh);
        let lock_surface = lock.create_lock_surface(surface, output, &self.qh);
        self.surfaces.push(Surf { surface: lock_surface, size: (0, 0) });
    }

    fn submit(&mut self) {
        if self.status == Status::Checking || self.password.is_empty() {
            return;
        }
        self.status = Status::Checking;
        self.redraw_all();
        let (user, password, tx) = (self.user.clone(), std::mem::take(&mut self.password), self.auth_tx.clone());
        std::thread::spawn(move || {
            let ok = pam::authenticate(&user, &password);
            let _ = tx.send(ok);
        });
    }

    fn auth_done(&mut self, ok: bool) {
        if ok {
            tracing::info!("unlocking");
            if let Some(lock) = self.session_lock.take() {
                lock.unlock();
            }
            // The unlock request must reach the compositor before we exit.
            let _ = self.conn.roundtrip();
            self.exit = true;
        } else {
            self.status = Status::Wrong;
            self.redraw_all();
        }
    }

    fn key(&mut self, event: KeyEvent) {
        if self.status == Status::Checking {
            return;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.submit(),
            Keysym::Escape => {
                self.password.clear();
                self.status = Status::Idle;
                self.redraw_all();
            }
            Keysym::BackSpace => {
                self.password.pop();
                self.status = Status::Idle;
                self.redraw_all();
            }
            _ => {
                if let Some(s) = event.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() && self.password.len() < 256 {
                        self.password.push_str(&printable);
                        self.status = Status::Idle;
                        self.redraw_all();
                    }
                }
            }
        }
    }

    fn redraw_all(&mut self) {
        for i in 0..self.surfaces.len() {
            self.draw(i);
        }
    }

    fn draw(&mut self, i: usize) {
        let (w, h) = self.surfaces[i].size;
        if w == 0 || h == 0 {
            return;
        }
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else {
            return;
        };
        let mut c = Canvas::new(canvas, w, h);
        // vertical gradient background: the surface colour fading towards a tint of the accent
        let (top, bottom) = (self.pal.surface, {
            let (a, b) = (self.pal.surface, self.pal.primary_container);
            [0, 1, 2].map(|k| (a[k] as f32 + (b[k] as f32 - a[k] as f32) * 0.45) as u8)
        });
        for y in 0..h as i32 {
            let t = y as f32 / h as f32;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let row = [mix(top[0], bottom[0]), mix(top[1], bottom[1]), mix(top[2], bottom[2]), 0xff];
            c.fill_row(y, row);
        }

        let (cx, w, h) = (w as i32 / 2, w as i32, h as i32);
        let now = chrono::Local::now();
        self.big.draw_centered(&mut c, cx, h * 36 / 100, &now.format("%H:%M").to_string(), self.pal.on_surface);
        self.medium.draw_centered(&mut c, cx, h * 36 / 100 + 90, &now.format("%A, %-d %B").to_string(), self.pal.on_surface_variant);

        // name + password field
        let field_y = h * 60 / 100;
        self.small.draw_centered(&mut c, cx, field_y - 30, &self.user, self.pal.on_surface_variant);
        let (fw, fh) = (340, 50);
        let fx = cx - fw / 2;
        c.rounded_rect(fx - 2, field_y - 2, fw + 4, fh + 4, (fh / 2 + 2) as f32, if self.status == Status::Wrong { self.pal.error } else { self.pal.primary });
        c.rounded_rect(fx, field_y, fw, fh, (fh / 2) as f32, self.pal.surface_container_high);
        let mid = field_y + fh / 2;
        match self.status {
            Status::Checking => self.small.draw_centered(&mut c, cx, mid, "Checking…", self.pal.on_surface_variant),
            _ if self.password.is_empty() => self.small.draw_centered(&mut c, cx, mid, "Password", self.pal.on_surface_variant),
            _ => {
                let n = self.password.chars().count().min(14) as i32;
                let spacing = 22;
                let start = cx - (n - 1) * spacing / 2;
                for k in 0..n {
                    c.circle(start + k * spacing, mid, 6, self.pal.on_surface);
                }
            }
        }
        if self.status == Status::Wrong {
            self.small.draw_centered(&mut c, cx, field_y + fh + 36, "Wrong password", self.pal.error);
        }
        let _ = w;

        if self.switch_cmd.is_some() {
            let (bx, by, bw, bh) = switch_button(w, h);
            if self.hover_switch {
                c.rounded_rect(bx, by, bw, bh, (bh / 2) as f32, self.pal.surface_container_high);
            }
            self.small.draw_centered(&mut c, bx + bw / 2, by + bh / 2, "Switch user", if self.hover_switch { self.pal.on_surface } else { self.pal.on_surface_variant });
        }

        let surface = self.surfaces[i].surface.wl_surface();
        surface.damage_buffer(0, 0, self.surfaces[i].size.0 as i32, self.surfaces[i].size.1 as i32);
        if buffer.attach_to(surface).is_ok() {
            surface.commit();
        }
    }
}

impl SessionLockHandler for Lock {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::info!("session locked");
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        tracing::warn!("the compositor refused to lock (already locked?)");
        self.exit_code = 1;
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: SessionLockSurface, configure: SessionLockSurfaceConfigure, _: u32) {
        if let Some(s) = self.surfaces.iter_mut().find(|s| s.surface.wl_surface() == surface.wl_surface()) {
            s.size = configure.new_size;
        }
        self.redraw_all();
    }
}

impl KeyboardHandler for Lock {
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

impl SeatHandler for Lock {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let handle = LOOP.with(|l| l.borrow().clone());
            self.keyboard = match handle {
                Some(h) => self
                    .seat_state
                    .get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|l: &mut Lock, _, ev| l.key(ev)))
                    .ok(),
                None => self.seat_state.get_keyboard(qh, &seat, None).ok(),
            };
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

impl CompositorHandler for Lock {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Lock {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // An output appeared after we locked: cover it too.
        if self.session_lock.is_some() {
            self.add_surface(&output);
        }
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Lock {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Lock);
delegate_output!(Lock);
delegate_shm!(Lock);
delegate_seat!(Lock);
delegate_keyboard!(Lock);
delegate_session_lock!(Lock);
delegate_registry!(Lock);

impl ProvidesRegistryState for Lock {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

/// The "Switch user" button's rectangle (x, y, w, h) for a surface of this size.
fn switch_button(w: i32, h: i32) -> (i32, i32, i32, i32) {
    let (bw, bh) = (160, 40);
    (w / 2 - bw / 2, h * 82 / 100, bw, bh)
}

impl Lock {
    fn switch_user(&mut self) {
        let Some(cmd) = &self.switch_cmd else { return };
        tracing::info!("switching user: {cmd}");
        match std::process::Command::new("sh").arg("-c").arg(cmd).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::error!("could not run the switch-user command: {err}"),
        }
    }
}

impl PointerHandler for Lock {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        if self.switch_cmd.is_none() {
            return;
        }
        for e in events {
            let Some(size) = self.surfaces.iter().find(|s| s.surface.wl_surface() == &e.surface).map(|s| s.size) else { continue };
            let (bx, by, bw, bh) = switch_button(size.0 as i32, size.1 as i32);
            let (px, py) = (e.position.0 as i32, e.position.1 as i32);
            let inside = px >= bx && px < bx + bw && py >= by && py < by + bh;
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    if inside != self.hover_switch {
                        self.hover_switch = inside;
                        self.redraw_all();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if self.hover_switch {
                        self.hover_switch = false;
                        self.redraw_all();
                    }
                }
                PointerEventKind::Press { button: 0x110, .. } if inside => self.switch_user(),
                _ => {}
            }
        }
    }
}

delegate_pointer!(Lock);

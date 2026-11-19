//! vesper-greeter: the login screen. A greetd greeter: it asks greetd to check the name and password (through PAM) and
//! to start the chosen session. Run it with `vesper --greeter` as greetd's session command.

mod greetd;
mod background;
mod login;
mod sessions;
mod users;

use std::{cell::RefCell, sync::mpsc, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
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
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_config::theme::Palette;
use vesper_gfx::{Canvas, Text};

use login::{Cmd, Ev};

type Rect = (i32, i32, i32, i32);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Field {
    User,
    Password,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    User,
    UserItem(usize),
    Password,
    Session,
    Shutdown,
    Reboot,
}

#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Checking,
    Info(String),
    Error(String),
}

struct Greeter {
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

    cmd_tx: mpsc::Sender<Cmd>,
    user: String,
    /// Accounts offered in the drop-down (empty = type the name).
    users: Vec<String>,
    menu: bool,
    /// Background image, blur and dim from the config, and the picture rendered for the current size.
    bg_cfg: (String, u32, u32),
    bg: Option<(u32, u32, Option<Vec<u8>>)>,
    password: String,
    field: Field,
    status: Status,
    /// greetd asked a further question: its text, and whether typing is hidden.
    prompt: Option<(String, bool)>,
    sessions: Vec<sessions::Session>,
    session: usize,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    big: Text,
    medium: Text,
    small: Text,
    pal: Palette,
}

thread_local! {
    static LOOP: RefCell<Option<LoopHandle<'static, Greeter>>> = const { RefCell::new(None) };
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    if let Err(err) = run() {
        eprintln!("vesper-greeter: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let config = vesper_config::Config::load();
    let sock = std::env::var("GREETD_SOCK").context("GREETD_SOCK is not set: run this as greetd's greeter (see docs/greeter.md)")?;

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-greeter"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (ev_tx, ev_rx) = channel::channel::<Ev>();
    login::spawn(sock, cmd_rx, ev_tx);

    let list = sessions::list();
    let (last_user, last_session) = sessions::last();
    let session = list.iter().position(|s| s.name == last_session).unwrap_or(0);
    let mut users = users::list();
    let user = if users.iter().any(|u| *u == last_user) { last_user.clone() } else { users.first().cloned().unwrap_or(last_user.clone()) };
    if !users.contains(&user) {
        users.clear();
    }
    let bg_path = if config.greeter.background.trim().is_empty() { config.wallpaper.path.clone() } else { config.greeter.background.clone() };
    let text = Text::new(&config.bar.font, 18.0);
    let mut app = Greeter {
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
        cmd_tx,
        field: if user.is_empty() { Field::User } else { Field::Password },
        user,
        users,
        menu: false,
        bg_cfg: (bg_path, config.greeter.blur, config.greeter.dim),
        bg: None,
        password: String::new(),
        status: Status::Idle,
        prompt: None,
        sessions: list,
        session,
        hits: Vec::new(),
        hover: None,
        big: text.resized(110.0),
        medium: text.resized(26.0),
        small: text.resized(17.0),
        pal: Palette::load(),
    };

    let mut event_loop: EventLoop<Greeter> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    LOOP.with(|l| *l.borrow_mut() = Some(handle.clone()));
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(ev_rx, |event, _, app| {
            if let channel::Event::Msg(ev) = event {
                app.on_event(ev);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, app| {
            app.redraw();
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow!("{e}"))?;

    while !app.exit {
        event_loop.dispatch(Duration::from_millis(100), &mut app)?;
    }
    Ok(())
}

impl Greeter {
    fn session_cmd(&self) -> (Vec<String>, Vec<String>) {
        let s = &self.sessions[self.session.min(self.sessions.len() - 1)];
        (vec!["sh".into(), "-c".into(), s.exec.clone()], vec!["XDG_SESSION_TYPE=wayland".into()])
    }

    fn submit(&mut self) {
        if self.status == Status::Checking {
            return;
        }
        if self.prompt.take().is_some() {
            let _ = self.cmd_tx.send(Cmd::Answer(std::mem::take(&mut self.password)));
            self.status = Status::Checking;
            self.redraw();
            return;
        }
        if self.user.trim().is_empty() {
            self.field = Field::User;
            self.redraw();
            return;
        }
        let (session, env) = self.session_cmd();
        let _ = self.cmd_tx.send(Cmd::Login { user: self.user.trim().to_string(), password: std::mem::take(&mut self.password), session, env });
        self.status = Status::Checking;
        self.redraw();
    }

    fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::Prompt { text, secret } => {
                self.prompt = Some((text, secret));
                self.password.clear();
                self.field = Field::Password;
                self.status = Status::Idle;
            }
            Ev::Info(m) => self.status = Status::Info(m),
            Ev::Failed(m) => {
                self.prompt = None;
                self.password.clear();
                self.field = Field::Password;
                self.status = Status::Error(m);
            }
            Ev::Started => {
                let s = &self.sessions[self.session.min(self.sessions.len() - 1)];
                sessions::remember(self.user.trim(), &s.name);
                self.exit = true;
                return;
            }
        }
        self.redraw();
    }

    fn key(&mut self, event: KeyEvent) {
        if self.status == Status::Checking {
            return;
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => {
                if self.field == Field::User && self.prompt.is_none() {
                    self.field = Field::Password;
                    self.redraw();
                } else {
                    self.submit();
                }
            }
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                if self.prompt.is_none() {
                    self.field = if self.field == Field::User { Field::Password } else { Field::User };
                    self.redraw();
                }
            }
            Keysym::Up | Keysym::Down if self.picks_user() => {
                let n = self.users.len();
                let i = self.users.iter().position(|u| *u == self.user).unwrap_or(0);
                self.user = self.users[if event.keysym == Keysym::Down { (i + 1) % n } else { (i + n - 1) % n }].clone();
                self.redraw();
            }
            Keysym::F3 => self.next_session(),
            Keysym::Escape => {
                if std::mem::take(&mut self.menu) {
                    self.redraw();
                    return;
                }
                if self.prompt.take().is_some() {
                    let _ = self.cmd_tx.send(Cmd::Cancel);
                }
                self.password.clear();
                self.status = Status::Idle;
                self.redraw();
            }
            Keysym::BackSpace if self.picks_user() => {}
            Keysym::BackSpace => {
                self.field_text().pop();
                self.status = Status::Idle;
                self.redraw();
            }
            _ if self.picks_user() => {}
            _ => {
                if let Some(s) = event.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() && self.field_text().len() < 256 {
                        self.field_text().push_str(&printable);
                        self.status = Status::Idle;
                        self.redraw();
                    }
                }
            }
        }
    }

    /// Is the user field a drop-down (so typing does nothing there)?
    fn picks_user(&self) -> bool {
        self.field == Field::User && self.prompt.is_none() && !self.users.is_empty()
    }

    fn field_text(&mut self) -> &mut String {
        if self.prompt.is_some() || self.field == Field::Password { &mut self.password } else { &mut self.user }
    }

    fn next_session(&mut self) {
        self.session = (self.session + 1) % self.sessions.len().max(1);
        self.redraw();
    }

    fn run(&self, program: &str, args: &[&str]) {
        if let Ok(mut c) = std::process::Command::new(program).args(args).spawn() {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }

    fn redraw(&mut self) {
        if !self.configured {
            return;
        }
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        self.pal = Palette::load();
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        if self.bg.as_ref().map(|b| (b.0, b.1)) != Some((w, h)) {
            let (path, blur, dim) = &self.bg_cfg;
            self.bg = Some((w, h, background::render(path, w, h, *blur, *dim)));
        }
        let picture = self.bg.as_ref().and_then(|b| b.2.as_deref()).filter(|p| p.len() == canvas.len());
        if let Some(p) = picture {
            canvas.copy_from_slice(p);
        }
        let mut c = Canvas::new(canvas, w, h);
        if picture.is_none() {
            let (top, bottom) = (self.pal.surface, {
                let (a, b) = (self.pal.surface, self.pal.primary_container);
                [0, 1, 2].map(|k| (a[k] as f32 + (b[k] as f32 - a[k] as f32) * 0.45) as u8)
            });
            for y in 0..h as i32 {
                let t = y as f32 / h as f32;
                let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
                c.fill_row(y, [mix(top[0], bottom[0]), mix(top[1], bottom[1]), mix(top[2], bottom[2]), 0xff]);
            }
        }
        let (cx, w, h)
 = (w as i32 / 2, w as i32, h as i32);
        let now = chrono::Local::now();
        self.big.draw_centered(&mut c, cx, h * 24 / 100, &now.format("%H:%M").to_string(), self.pal.on_surface);
        self.medium.draw_centered(&mut c, cx, h * 24 / 100 + 84, &now.format("%A, %-d %B").to_string(), self.pal.on_surface_variant);

        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let (fw, fh) = (360, 50);
        let fx = cx - fw / 2;
        let mut y = h * 50 / 100;
        let mut menu_y = 0;
        let field = |c: &mut Canvas, hits: &mut Vec<(Rect, Hit)>, y: i32, focused: bool, hit: Hit, hot: bool| {
            let ring = if focused { self.pal.primary } else if hot { self.pal.on_surface_variant } else { self.pal.surface_container_high };
            c.rounded_rect(fx - 2, y - 2, fw + 4, fh + 4, (fh / 2 + 2) as f32, ring);
            c.rounded_rect(fx, y, fw, fh, (fh / 2) as f32, self.pal.surface_container_high);
            hits.push(((fx, y, fw, fh), hit));
        };
        // user name (hidden while greetd asks a further question)
        if self.prompt.is_none() {
            field(&mut c, &mut hits, y, self.field == Field::User, Hit::User, self.hover == Some(Hit::User));
            let shown = if self.user.is_empty() { ("Username".to_string(), self.pal.on_surface_variant) } else { (self.user.clone(), self.pal.on_surface) };
            let arrow = if self.users.is_empty() { "" } else { "  \u{25be}" };
            self.small.draw_centered(&mut c, cx, y + fh / 2, &format!("{}{arrow}", self.small.fit(&shown.0, fw - 80)), shown.1);
            menu_y = y + fh + 6;
            y += fh + 14;
        } else if let Some((text, _)) = &self.prompt {
            self.small.draw_centered(&mut c, cx, y + 14, &self.small.fit(text.trim().trim_end_matches(':'), fw), self.pal.on_surface_variant);
            y += 34;
        }
        // password
        field(&mut c, &mut hits, y, self.field == Field::Password, Hit::Password, self.hover == Some(Hit::Password));
        let mid = y + fh / 2;
        let reveal = matches!(&self.prompt, Some((_, false)));
        if self.status == Status::Checking {
            self.small.draw_centered(&mut c, cx, mid, "Signing in…", self.pal.on_surface_variant);
        } else if self.password.is_empty() {
            self.small.draw_centered(&mut c, cx, mid, if self.prompt.is_some() { "Answer" } else { "Password" }, self.pal.on_surface_variant);
        } else if reveal {
            self.small.draw_centered(&mut c, cx, mid, &self.small.fit(&self.password, fw - 40), self.pal.on_surface);
        } else {
            let n = self.password.chars().count().min(14) as i32;
            let start = cx - (n - 1) * 22 / 2;
            for k in 0..n {
                c.circle(start + k * 22, mid, 6, self.pal.on_surface);
            }
        }
        y += fh + 14;
        // the session
        let s = &self.sessions[self.session.min(self.sessions.len() - 1)];
        let label = format!("{}{}", s.name, if self.sessions.len() > 1 { "  \u{25be}" } else { "" });
        let sw = self.small.width(&label) + 36;
        if self.hover == Some(Hit::Session) && self.sessions.len() > 1 {
            c.rounded_rect(cx - sw / 2, y, sw, 36, 18.0, self.pal.surface_container_high);
        }
        self.small.draw_centered(&mut c, cx, y + 18, &label, self.pal.on_surface_variant);
        if self.sessions.len() > 1 {
            hits.push(((cx - sw / 2, y, sw, 36), Hit::Session));
        }
        y += 56;
        match &self.status {
            Status::Error(m) => self.small.draw_centered(&mut c, cx, y, &self.small.fit(m, w - 40), self.pal.error),
            Status::Info(m) => self.small.draw_centered(&mut c, cx, y, &self.small.fit(m, w - 40), self.pal.on_surface_variant),
            _ => {}
        }
        self.small.resized(14.0).draw_centered(&mut c, cx, h - 34, "Tab: next field   F3: change session", with_alpha(self.pal.on_surface_variant, 0.7));

        // power buttons, bottom right
        for (i, (label, hit)) in [("Restart", Hit::Reboot), ("Shut down", Hit::Shutdown)].into_iter().enumerate() {
            let bw = self.small.width(label) + 36;
            let shut_w = self.small.width("Shut down") + 36;
            let bx = if i == 0 { w - 24 - shut_w - 8 - bw } else { w - 24 - bw };
            let by = h - 64;
            if self.hover == Some(hit) {
                c.rounded_rect(bx, by, bw, 36, 18.0, self.pal.surface_container_high);
            }
            self.small.draw_centered(&mut c, bx + bw / 2, by + 18, label, self.pal.on_surface_variant);
            hits.push(((bx, by, bw, 36), hit));
        }
        if self.menu && self.prompt.is_none() {
            let rows = self.users.len() as i32;
            c.rounded_rect(fx - 2, menu_y - 2, fw + 4, rows * 40 + 8, 18.0, self.pal.surface_container_high);
            c.rounded_rect(fx, menu_y, fw, rows * 40 + 4, 16.0, self.pal.surface);
            let mut items = Vec::new();
            for (i, name) in self.users.iter().enumerate() {
                let ry = menu_y + 2 + i as i32 * 40;
                let hot = self.hover == Some(Hit::UserItem(i));
                if hot || *name == self.user {
                    c.rounded_rect(fx + 4, ry, fw - 8, 36, 14.0, if hot { self.pal.surface_container_high } else { self.pal.primary_container });
                }
                self.small.draw_centered(&mut c, cx, ry + 18, &self.small.fit(name, fw - 40), self.pal.on_surface);
                items.push(((fx, ry, fw, 36), Hit::UserItem(i)));
            }
            hits.splice(0..0, items); // the list is on top, so it is hit first
        }
        self.hits = hits;

        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            surface.commit();
        }
    }

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        let (x, y) = (x as i32, y as i32);
        self.hits.iter().find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(_, h)| *h)
    }
}

fn with_alpha(c: [u8; 4], a: f32) -> [u8; 4] {
    [c[0], c[1], c[2], (c[3] as f32 * a) as u8]
}

impl LayerShellHandler for Greeter {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        self.size = c.new_size;
        self.configured = true;
        self.redraw();
    }
}

impl PointerHandler for Greeter {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            match e.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    let hover = self.hit_at(e.position.0, e.position.1);
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
                PointerEventKind::Press { button: 0x110, .. } => match self.hit_at(e.position.0, e.position.1) {
                    Some(Hit::User) => {
                        self.field = Field::User;
                        self.menu = !self.users.is_empty() && !self.menu;
                        self.redraw();
                    }
                    Some(Hit::UserItem(i)) => {
                        if let Some(u) = self.users.get(i) {
                            self.user = u.clone();
                        }
                        self.menu = false;
                        self.field = Field::Password;
                        self.redraw();
                    }
                    Some(Hit::Password) => {
                        self.menu = false;
                        self.field = Field::Password;
                        self.redraw();
                    }
                    Some(Hit::Session) => self.next_session(),
                    Some(Hit::Shutdown) => self.run("systemctl", &["poweroff"]),
                    Some(Hit::Reboot) => self.run("systemctl", &["reboot"]),
                    None => {
                        if std::mem::take(&mut self.menu) {
                            self.redraw();
                        }
                    }
                },
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for Greeter {
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

impl SeatHandler for Greeter {
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
                Some(h) => self.seat_state.get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|g: &mut Greeter, _, ev| g.key(ev))).ok(),
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

impl CompositorHandler for Greeter {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Greeter {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Greeter {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Greeter);
delegate_output!(Greeter);
delegate_shm!(Greeter);
delegate_seat!(Greeter);
delegate_keyboard!(Greeter);
delegate_pointer!(Greeter);
delegate_layer!(Greeter);
delegate_registry!(Greeter);

impl ProvidesRegistryState for Greeter {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

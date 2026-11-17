//! vesper-polkit: the polkit authentication agent. Programs that need admin rights (pkexec, mounting, package tools,
//! GNOME-style apps) ask polkitd, which asks us; we show a password dialog styled like the top bar and check the
//! password with polkit's own setuid helper (`polkit-agent-helper-1`, see `helper.rs`).
//!
//! Enter authenticates, Esc cancels, Tab switches between users when polkit allows several.

mod agent;
mod helper;

use std::{collections::VecDeque, num::NonZeroU32, path::PathBuf, time::{Duration, Instant}};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel::{self, Channel, Sender},
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
use vesper_config::theme::{alpha, with_alpha, Palette};
use vesper_gfx::{Canvas, Rgba, Text};

use agent::{Failure, Msg, Request};
use helper::{Helper, Line};

const W: u32 = 440;
const PAD: i32 = 18;
const LINE_H: i32 = 20;
const FIELD_H: i32 = 42;
const BTN_H: i32 = 36;
const BTN_LEFT: u32 = 0x110;
const SHAKE: Duration = Duration::from_millis(450);

type Rect = (i32, i32, i32, i32);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Ok,
    Cancel,
}

/// The request being shown.
struct Active {
    req: Request,
    ident: usize,
    text: String,
    /// The helper is waiting for an answer: (what it asked, answer hidden?).
    prompt: Option<(String, bool)>,
    info: Option<String>,
    error: Option<String>,
    /// A password went to the helper and its verdict is pending.
    checking: bool,
    /// Enter was pressed before the helper asked.
    submit_when_ready: bool,
    helper: Option<Helper>,
    helper_id: u64,
    message_lines: Vec<String>,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    compositor: CompositorState,
    layer_shell: LayerShell,
    qh: QueueHandle<App>,
    handle: LoopHandle<'static, App>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,

    layer: Option<LayerSurface>,
    height: u32,
    configured: bool,

    queue: VecDeque<Request>,
    active: Option<Active>,
    helper_path: Option<PathBuf>,
    helper_tx: Sender<(u64, Line)>,
    next_helper: u64,

    title: Text,
    text: Text,
    pal: Palette,
    ctrl: bool,
    hits: Vec<(Rect, Hit)>,
    hover: Option<Hit>,
    shake: Option<Instant>,

    // the top bar's look
    radius: f32,
    border_w: f32,
    border: Option<Rgba>,
    border_blend: bool,
    bg_alpha: u8,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let helper_path = helper::find_helper();
    match &helper_path {
        Some(p) => tracing::info!("using {}", p.display()),
        None => tracing::warn!("polkit-agent-helper-1 not found: install polkit (or set VESPER_POLKIT_HELPER)"),
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let (helper_tx, helper_rx) = channel::channel::<(u64, Line)>();
    let (msg_tx, msg_rx): (Sender<Msg>, Channel<Msg>) = channel::channel();

    let bar = &config.bar;
    let title = Text::new(&bar.font, 19.0);
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(W as usize * 400 * 4, &shm)?,
        shm,
        compositor,
        layer_shell,
        qh: qh.clone(),
        handle: handle.clone(),
        keyboard: None,
        pointer: None,
        layer: None,
        height: 0,
        configured: false,
        queue: VecDeque::new(),
        active: None,
        helper_path,
        helper_tx,
        next_helper: 1,
        text: title.resized(14.5),
        title,
        pal: Palette::load(),
        ctrl: false,
        hits: Vec::new(),
        hover: None,
        shake: None,
        radius: bar.radius.clamp(6, 30) as f32,
        border_w: bar.border_width.clamp(0, 6).max(1) as f32,
        border: vesper_config::theme::hex(&bar.border_color),
        border_blend: bar.border_blend,
        bg_alpha: (bar.opacity.clamp(20, 100) * 255 / 100) as u8,
    };

    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;
    KEY_LOOP.with(|k| *k.borrow_mut() = Some(handle.clone()));
    handle
        .insert_source(msg_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg(m) = ev {
                match m {
                    Msg::Begin(req) => app.begin(req),
                    Msg::Cancel(cookie) => app.cancel_cookie(&cookie),
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(helper_rx, |ev, _, app: &mut App| {
            if let channel::Event::Msg((id, line)) = ev {
                app.helper_line(id, line);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    std::thread::Builder::new().name("polkit-dbus".into()).spawn(move || agent::run(msg_tx))?;

    loop {
        if let Err(err) = event_loop.dispatch(None, &mut app) {
            tracing::info!("compositor connection closed: {err}");
            break;
        }
    }
    Ok(())
}

thread_local! {
    static KEY_LOOP: std::cell::RefCell<Option<LoopHandle<'static, App>>> = const { std::cell::RefCell::new(None) };
}

/// Break `s` into lines no wider than `max` (by `width`), at most `max_lines`; the last one gets "…" if cut.
pub fn wrap(s: &str, max: i32, max_lines: usize, width: impl Fn(&str) -> i32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let try_line = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if width(&try_line) <= max || cur.is_empty() {
            cur = try_line;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            last.push('…');
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

impl App {
    fn begin(&mut self, req: Request) {
        tracing::info!("authentication requested for {} (as {:?})", req.action_id, req.identities.iter().map(|i| &i.name).collect::<Vec<_>>());
        self.queue.push_back(req);
        if self.active.is_none() {
            self.show_next();
        }
    }

    /// polkit withdrew a request (the program died or was answered elsewhere).
    fn cancel_cookie(&mut self, cookie: &str) {
        if self.active.as_ref().is_some_and(|a| a.req.cookie == cookie) {
            self.finish(Err(Failure::Cancelled));
        } else if let Some(i) = self.queue.iter().position(|r| r.cookie == cookie) {
            if let Some(r) = self.queue.remove(i) {
                let _ = r.reply.try_send(Err(Failure::Cancelled));
            }
        }
    }

    fn show_next(&mut self) {
        let Some(req) = self.queue.pop_front() else { return };
        self.pal = Palette::load();
        let message = if req.message.trim().is_empty() { req.action_id.clone() } else { req.message.clone() };
        let message_lines = wrap(&message, W as i32 - 2 * PAD, 4, |s| self.text.width(s));
        let ident = 0;
        self.active = Some(Active {
            req,
            ident,
            text: String::new(),
            prompt: None,
            info: None,
            error: None,
            checking: false,
            submit_when_ready: false,
            helper: None,
            helper_id: 0,
            message_lines,
        });
        self.start_helper();

        let lines = self.active.as_ref().map_or(1, |a| a.message_lines.len()) as i32;
        let h = (PAD + 30 + lines * LINE_H + 26 + 10 + FIELD_H + 26 + 8 + BTN_H + PAD) as u32;
        self.height = h;
        self.configured = false;
        self.hover = None;
        self.shake = None;
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("vesper-polkit"), None);
        layer.set_anchor(Anchor::empty()); // centred
        layer.set_size(W, h);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.layer = Some(layer);
    }

    /// (Re)start the helper for the chosen user.
    fn start_helper(&mut self) {
        let Some(path) = self.helper_path.clone() else {
            if let Some(a) = &mut self.active {
                a.error = Some("polkit-agent-helper-1 is not installed".into());
            }
            return;
        };
        let id = self.next_helper;
        self.next_helper += 1;
        let tx = self.helper_tx.clone();
        let Some(a) = &mut self.active else { return };
        a.helper = None; // kills the old one
        a.helper_id = id;
        a.prompt = None;
        a.checking = false;
        let user = a.req.identities[a.ident].name.clone();
        match Helper::spawn(&path, &user, &a.req.cookie, id, tx) {
            Ok(h) => a.helper = Some(h),
            Err(e) => {
                tracing::error!("could not start {}: {e}", path.display());
                a.error = Some(format!("could not start the authentication helper: {e}"));
            }
        }
    }

    fn helper_line(&mut self, id: u64, line: Line) {
        let Some(a) = self.active.as_mut().filter(|a| a.helper_id == id) else { return };
        tracing::debug!("helper: {line:?}");
        match line {
            Line::PromptHidden(_) | Line::PromptVisible(_) => {
                let (p, hidden) = match line {
                    Line::PromptHidden(p) => (p, true),
                    Line::PromptVisible(p) => (p, false),
                    _ => unreachable!(),
                };
                a.prompt = Some((p.trim().to_string(), hidden));
                if a.submit_when_ready && !a.text.is_empty() {
                    a.submit_when_ready = false;
                    self.submit();
                }
            }
            Line::Error(m) if m.contains("setuid root") => a.error = Some("polkit-agent-helper-1 is not setuid root (reinstall polkit)".into()),
            Line::Error(m) => a.error = Some(m),
            Line::Info(m) => a.info = Some(m),
            Line::Success => {
                self.finish(Ok(()));
                return;
            }
            Line::Failure => {
                let was_checking = a.checking;
                a.error.get_or_insert_with(|| "Sorry, that didn't work. Please try again.".into());
                a.text.clear();
                a.helper = None;
                a.prompt = None;
                a.checking = false;
                if was_checking {
                    self.shake_now();
                    self.start_helper(); // a fresh helper asks again
                }
            }
            Line::Eof => {
                if a.helper.is_some() {
                    a.helper = None;
                    a.prompt = None;
                    a.checking = false;
                    a.error.get_or_insert_with(|| "The authentication helper stopped. Press Enter to try again.".into());
                }
            }
            Line::Other(s) => tracing::debug!("unexpected helper output: {s}"),
        }
        self.redraw();
    }

    fn shake_now(&mut self) {
        self.shake = Some(Instant::now());
        let _ = self.handle.insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, app: &mut App| {
            app.redraw();
            match app.shake {
                Some(t) if t.elapsed() < SHAKE => TimeoutAction::ToDuration(Duration::from_millis(16)),
                _ => {
                    app.shake = None;
                    app.redraw();
                    TimeoutAction::Drop
                }
            }
        });
    }

    /// Send what was typed to the helper.
    fn submit(&mut self) {
        let Some(a) = &mut self.active else { return };
        if a.checking {
            return;
        }
        if a.helper.is_none() {
            // it exited (setup problem or a previous failure): try again, and send once it asks
            a.error = None;
            a.submit_when_ready = true;
            self.start_helper();
            self.redraw();
            return;
        }
        if a.prompt.is_none() {
            a.submit_when_ready = true;
            return;
        }
        let text = std::mem::take(&mut a.text);
        let sent = a.helper.as_mut().is_some_and(|h| h.answer(&text).is_ok());
        a.prompt = None;
        a.error = None;
        if sent {
            a.checking = true;
        } else {
            a.error = Some("The authentication helper stopped. Press Enter to try again.".into());
            a.helper = None;
        }
        self.redraw();
    }

    fn switch_user(&mut self) {
        let Some(a) = &mut self.active else { return };
        let n = a.req.identities.len();
        if n < 2 || a.checking {
            return;
        }
        a.ident = (a.ident + 1) % n;
        a.text.clear();
        a.error = None;
        a.info = None;
        self.start_helper();
        self.redraw();
    }

    /// Close the dialog and tell polkit how it went; show the next request if one is waiting.
    fn finish(&mut self, result: Result<(), Failure>) {
        if let Some(a) = self.active.take() {
            let _ = a.req.reply.try_send(result);
        }
        self.layer = None; // destroys the surface
        self.configured = false;
        self.hits.clear();
        self.shake = None;
        self.show_next();
    }

    fn key(&mut self, event: KeyEvent) {
        if self.active.is_none() {
            return;
        }
        match event.keysym {
            Keysym::Escape => self.finish(Err(Failure::Cancelled)),
            Keysym::Return | Keysym::KP_Enter => self.submit(),
            Keysym::Tab | Keysym::ISO_Left_Tab => self.switch_user(),
            Keysym::BackSpace => {
                if let Some(a) = &mut self.active {
                    a.text.pop();
                    self.redraw();
                }
            }
            Keysym::u | Keysym::U if self.ctrl => {
                if let Some(a) = &mut self.active {
                    a.text.clear();
                    self.redraw();
                }
            }
            _ if self.ctrl => {}
            _ => {
                if let (Some(s), Some(a)) = (event.utf8, &mut self.active) {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() && !a.checking {
                        a.text.push_str(&printable);
                        a.error = None;
                        self.redraw();
                    }
                }
            }
        }
    }

    fn redraw(&mut self) {
        let (Some(layer), true) = (&self.layer, self.configured) else { return };
        let Some(a) = &self.active else { return };
        let (w, h) = (W, self.height);
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else {
            tracing::warn!("could not allocate a {w}x{h} buffer");
            return;
        };
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let p = &self.pal;
        let accent = p.primary;
        let iw = w as i32 - 2 * PAD;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        c.rounded_rect(0, 0, w as i32, h as i32, self.radius, alpha(p.surface, self.bg_alpha));
        let mut border = self.border.unwrap_or(accent);
        if self.border_blend {
            border[3] = (border[3] as u32 * self.bg_alpha as u32 / 255) as u8;
        }
        c.rounded_outline(0, 0, w as i32, h as i32, self.radius, self.border_w, border);

        let mut y = PAD;
        self.title.draw(&mut c, PAD, y + 12, "Authentication required", accent);
        y += 30;
        for l in &a.message_lines {
            self.text.draw(&mut c, PAD, y + LINE_H / 2, l, p.on_surface);
            y += LINE_H;
        }
        let who = &a.req.identities[a.ident].name;
        let multi = a.req.identities.len() > 1;
        let sub = if multi { format!("As {who}  ·  Tab switches user") } else { format!("As {who}") };
        self.text.draw(&mut c, PAD, y + 16, &self.text.fit(&sub, iw), with_alpha(p.on_surface, 0.6));
        y += 26 + 10;

        // password field (shakes on a wrong password)
        let dx = match self.shake {
            Some(t) => {
                let f = (t.elapsed().as_secs_f32() / SHAKE.as_secs_f32()).clamp(0.0, 1.0);
                ((f * 6.0 * std::f32::consts::TAU).sin() * 9.0 * (1.0 - f)) as i32
            }
            None => 0,
        };
        let fx = PAD + dx;
        c.rounded_rect(fx, y, iw, FIELD_H, 10.0, with_alpha(p.on_surface, 0.10));
        let ring = if self.shake.is_some() { p.error } else { accent };
        c.rounded_outline(fx, y, iw, FIELD_H, 10.0, 1.5, ring);
        let label = a.prompt.as_ref().map(|p| p.0.clone()).unwrap_or_default();
        if a.text.is_empty() {
            let hint = if a.checking {
                "Checking…".to_string()
            } else if label.is_empty() {
                "Password".to_string()
            } else {
                label.trim_end_matches([':', ' ']).to_string()
            };
            self.text.draw(&mut c, fx + 14, y + FIELD_H / 2, &hint, with_alpha(p.on_surface, 0.45));
        } else {
            let hidden = a.prompt.as_ref().is_none_or(|p| p.1);
            let shown = if hidden { "\u{2022}".repeat(a.text.chars().count().min(40)) } else { self.text.fit(&a.text, iw - 40) };
            self.text.draw(&mut c, fx + 14, y + FIELD_H / 2, &format!("{shown}\u{258f}"), p.on_surface);
        }
        y += FIELD_H;
        let status = if let Some(e) = &a.error {
            Some((e.clone(), p.error))
        } else {
            a.info.clone().map(|i| (i, with_alpha(p.on_surface, 0.7)))
        };
        if let Some((m, col)) = status {
            self.text.draw(&mut c, PAD, y + 14, &self.text.fit(&m, iw), col);
        }
        y += 26 + 8;

        let half = (iw - 8) / 2;
        for (i, (label, hit)) in [("Authenticate", Hit::Ok), ("Cancel", Hit::Cancel)].into_iter().enumerate() {
            let bx = PAD + i as i32 * (half + 8);
            let primary = i == 0;
            let hot = self.hover == Some(hit);
            let fill = if primary { if hot { with_alpha(accent, 0.85) } else { accent } } else if hot { with_alpha(p.on_surface, 0.16) } else { with_alpha(p.on_surface, 0.08) };
            c.rounded_rect(bx, y, half, BTN_H, 10.0, fill);
            self.text.draw_centered(&mut c, bx + half / 2, y + BTN_H / 2, label, if primary { p.on_primary } else { p.on_surface });
            hits.push(((bx, y, half, BTN_H), hit));
        }
        self.hits = hits;

        layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
        }
    }

    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        let (x, y) = (x as i32, y as i32);
        self.hits.iter().find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(_, h)| *h)
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor took the surface away: treat it as dismissed.
        if self.active.is_some() {
            self.finish(Err(Failure::Cancelled));
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        self.height = NonZeroU32::new(c.new_size.1).map_or(self.height, NonZeroU32::get);
        self.configured = true;
        self.redraw();
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if self.layer.as_ref().map(|l| l.wl_surface()) != Some(&e.surface) {
                continue;
            }
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
                PointerEventKind::Press { button: BTN_LEFT, .. } => match self.hit_at(e.position.0, e.position.1) {
                    Some(Hit::Ok) => self.submit(),
                    Some(Hit::Cancel) => {
                        self.finish(Err(Failure::Cancelled));
                        return;
                    }
                    None => {}
                },
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
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, modifiers: Modifiers, _: RawModifiers, _: u32) {
        self.ctrl = modifiers.ctrl;
    }
}

impl SeatHandler for App {
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
                Some(h) => self.seat_state.get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|a: &mut App, _, ev| a.key(ev))).ok(),
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
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_on_words_and_truncates() {
        let w = |s: &str| s.chars().count() as i32;
        assert_eq!(wrap("Authentication is needed to run foo as the super user", 20, 4, w), vec!["Authentication is", "needed to run foo as", "the super user"]);
        assert_eq!(wrap("a b c d e f g h", 3, 2, w), vec!["a b", "c d…"]);
        assert_eq!(wrap("", 10, 2, w), vec![String::new()]);
        assert_eq!(wrap("supercalifragilistic", 5, 3, w), vec!["supercalifragilistic"]);
    }
}

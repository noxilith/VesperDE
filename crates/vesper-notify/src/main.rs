//! vesper-notify: shows notifications as cards in the top-right corner.
//!
//! Click a card to dismiss it. Normal notifications expire after a few seconds; critical ones stay until clicked.

mod dbus;

use std::{
    num::NonZeroU32,
    sync::atomic::AtomicU32,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use dbus::{emit_action, emit_closed, Control, strip_markup, Msg, Notification, Reason, Service};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm,
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
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
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
use vesper_config::theme::{alpha, Palette};
use vesper_gfx::{Canvas, Image, Rgba, Text};

const BTN_LEFT: u32 = 0x110;
const WIDTH: i32 = 360;
const CARD_H: i32 = 78;
const ACT_H: i32 = 38;
const CENTER_W: i32 = 400;
const GAP: i32 = 8;
const ICON: i32 = 40;
const PAD: i32 = 14;
const MAX_VISIBLE: usize = 5;

const TILE_TEXT: Rgba = [0xff, 0xff, 0xff, 0xf0];

type Rect = (i32, i32, i32, i32);

#[derive(Clone)]
enum Hit {
    Dismiss(u32),
    Action(u32, String),
    Close,
    Clear,
    Dnd,
    Forget(u64),
}

struct Past {
    key: u64,
    n: Notification,
    at: Instant,
}

struct Center {
    layer: LayerSurface,
    configured: bool,
    size: (u32, u32),
    scroll: i32,
    hits: Vec<(Rect, Hit)>,
}

struct Card {
    n: Notification,
    deadline: Option<Instant>,
    icon: Option<Image>,
}

struct Notify {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    compositor: CompositorState,
    layer_shell: LayerShell,
    qh: QueueHandle<Notify>,
    pointer: Option<wl_pointer::WlPointer>,

    layer: Option<LayerSurface>,
    configured: bool,
    /// The size the surface currently has, or has been asked to have.
    size: (u32, u32),
    top_margin: i32,

    cards: Vec<Card>,
    hits: Vec<(Rect, Hit)>,
    history: Vec<Past>,
    history_key: u64,
    published: String,
    center: Option<Center>,
    last_center_draw: Instant,
    dnd: bool,
    config: vesper_config::Config,
    config_stamp: Option<std::time::SystemTime>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    icons: Icons,
    apps: Apps,
    pal: Palette,
    palette_stamp: Option<std::time::SystemTime>,
    text: Text,
    small: Text,
    dbus: zbus::blocking::Connection,
}

fn main() -> Result<()> {
    // `vesper-notify send SUMMARY [BODY]`: show one notification through whatever daemon is running, then exit.
    let args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("center" | "clear" | "dnd")) {
        let conn = zbus::blocking::Connection::session().context("connecting to the session bus")?;
        let call = |method: &str| conn.call_method(Some("org.vesper.Notify"), "/org/vesper/Notify", Some("org.vesper.Notify"), method, &());
        match args[1].as_str() {
            "center" => call("ToggleCenter").context("vesper-notify is not running")?,
            "clear" => call("ClearHistory").context("vesper-notify is not running")?,
            _ => conn
                .call_method(Some("org.vesper.Notify"), "/org/vesper/Notify", Some("org.vesper.Notify"), "SetDoNotDisturb", &(args.get(2).map(String::as_str) != Some("off")))
                .context("vesper-notify is not running")?,
        };
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("send") {
        let summary = args.get(2).map(String::as_str).unwrap_or("");
        let body = args.get(3).map(String::as_str).unwrap_or("");
        let conn = zbus::blocking::Connection::session().context("connecting to the session bus")?;
        let hints: std::collections::HashMap<&str, zbus::zvariant::Value> = std::collections::HashMap::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Vesper", 0u32, "", summary, body, Vec::<String>::new(), hints, -1i32),
        )
        .context("no notification daemon answered")?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();

    // Wayland first, so we only claim the bus name once we can actually show things.
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let (tx, rx) = channel::channel::<Msg>();
    let dbus = zbus::blocking::connection::Builder::session()
        .context("connecting to the session bus (is D-Bus running?)")?
        .name("org.freedesktop.Notifications")
        .map_err(|e| anyhow!("{e}"))?
                .name("org.vesper.Notify")
        .map_err(|e| anyhow!("{e}"))?
        .serve_at("/org/vesper/Notify", Control { tx: tx.clone() })
        .map_err(|e| anyhow!("{e}"))?
.serve_at("/org/freedesktop/Notifications", Service { tx, next_id: AtomicU32::new(1) })
        .map_err(|e| anyhow!("{e}"))?
        .build()
        .context("claiming org.freedesktop.Notifications (is another notification daemon, e.g. mako, running?)")?;
    tracing::info!("serving org.freedesktop.Notifications");

    let text = Text::new(&config.bar.font, 15.0);
    let mut app = Notify {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(WIDTH as usize * 400 * 4, &shm)?,
        shm,
        compositor,
        layer_shell,
        qh: qh.clone(),
        pointer: None,
        layer: None,
        configured: false,
        size: (0, 0),
        top_margin: config.bar.height as i32 + 8,
        cards: Vec::new(),
        hits: Vec::new(),
        history: Vec::new(),
        history_key: 0,
        published: String::new(),
        center: None,
        dnd: config.notifications.do_not_disturb,
        last_center_draw: Instant::now(),
        config_stamp: vesper_config::Config::stamp(),
        keyboard: None,
        config: config.clone(),
        icons: Icons::default(),
        apps: Apps::load(),
        pal: Palette::load(),
        palette_stamp: Palette::stamp(),
        small: text.resized(13.0),
        text,
        dbus,
    };

    let mut event_loop: EventLoop<Notify> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Notify(n) => app.add(n),
                    Msg::Close(id) => app.remove(id, Reason::Closed),
                    Msg::ToggleCenter => app.toggle_center(),
                    Msg::ClearHistory => {
                        app.history.clear();
                        app.center_redraw();
                    }
                    Msg::SetDnd(on) => {
                        app.dnd = on;
                        app.center_redraw();
                    }
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    handle
        .insert_source(Timer::from_duration(Duration::from_millis(500)), |_, _, app| {
            app.expire();
            app.refresh_palette();
            app.refresh_config();
            app.publish_state();
            app.center_redraw_if_stale();
            TimeoutAction::ToDuration(Duration::from_millis(500))
        })
        .map_err(|e| anyhow!("{e}"))?;

    loop {
        if let Err(err) = event_loop.dispatch(None, &mut app) {
            tracing::info!("compositor connection closed: {err}");
            break;
        }
    }
    Ok(())
}

impl Notify {
    /// The top bar's look for popups and cards: (corner radius, fill alpha, border width, border colour).
    fn bar_look(config: &vesper_config::Config, pal: &Palette) -> (f32, u8, f32, vesper_gfx::Rgba) {
        let b = &config.bar;
        let fill = (b.opacity.clamp(40, 100) * 255 / 100) as u8;
        let mut border = vesper_config::theme::hex(&b.border_color).unwrap_or(pal.primary);
        if b.border_blend {
            border[3] = (border[3] as u32 * fill as u32 / 255) as u8;
        }
        (b.radius.clamp(8, 28) as f32, fill, b.border_width.clamp(0, 6).max(1) as f32, border)
    }

    /// Do-not-disturb right now: switched on by hand, or by the schedule.
    fn dnd_now(&self) -> bool {
        use chrono::Timelike;
        self.dnd || self.config.notifications.scheduled_at(chrono::Local::now().hour())
    }

    fn add(&mut self, n: Notification) {
        self.remember(&n);
        if self.dnd_now() && n.urgency < 2 {
            return; // kept in the history, not shown
        }
        let default = self.config.notifications.default_timeout;
        let deadline = match n.timeout_ms {
            0 => None,
            _ if n.urgency >= 2 => None, // critical notifications wait for you
            t if t > 0 => Some(Instant::now() + Duration::from_millis(t as u64)),
            _ if default == 0 => None,
            _ => Some(Instant::now() + Duration::from_secs(default as u64)),
        };
        let icon = self.load_icon(&n);
        let card = Card { n, deadline, icon };
        match self.cards.iter().position(|c| c.n.id == card.n.id) {
            Some(i) if card.n.replaces => self.cards[i] = card,
            _ => self.cards.insert(0, card), // newest on top
        }
        self.relayout();
    }

    fn load_icon(&mut self, n: &Notification) -> Option<Image> {
        // The icon hint may be a path, an icon name, or just the app's desktop id.
        let name = if n.icon.is_empty() { n.app_name.clone() } else { n.icon.clone() };
        let mapped = self.apps.find(&name).map(|a| a.icon.clone()).filter(|i| !i.is_empty());
        self.icons.get(&name, ICON).or_else(|| mapped.and_then(|i| self.icons.get(&i, ICON)))
    }

    fn remove(&mut self, id: u32, reason: Reason) {
        if let Some(i) = self.cards.iter().position(|c| c.n.id == id) {
            self.cards.remove(i);
            emit_closed(&self.dbus, id, reason);
            self.relayout();
        }
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let gone: Vec<u32> =
            self.cards.iter().filter(|c| c.deadline.is_some_and(|d| d <= now)).map(|c| c.n.id).collect();
        for id in gone {
            self.remove(id, Reason::Expired);
        }
    }

    /// Pick up new matugen colours.
    fn refresh_palette(&mut self) {
        let stamp = Palette::stamp();
        if stamp != self.palette_stamp {
            self.palette_stamp = stamp;
            let pal = Palette::load();
            if pal != self.pal {
                self.pal = pal;
                self.redraw();
            }
        }
    }

    fn card_height(c: &Card) -> i32 {
        CARD_H + if c.n.actions.is_empty() { 0 } else { ACT_H }
    }

    /// Top and height of each visible popup card.
    fn popup_rows(&self) -> Vec<(i32, i32)> {
        if self.center.is_some() {
            return Vec::new(); // the centre already lists them
        }
        let mut y = 0;
        self.cards
            .iter()
            .take(MAX_VISIBLE)
            .map(|c| {
                let h = Self::card_height(c);
                let row = (y, h);
                y += h + GAP;
                row
            })
            .collect()
    }

    /// Create, resize or destroy the layer surface to fit the cards.
    fn relayout(&mut self) {
        let rows = self.popup_rows();
        let Some(&(last_y, last_h)) = rows.last() else {
            self.layer = None; // dropping the surface unmaps it
            self.configured = false;
            self.size = (0, 0);
            self.hits.clear();
            return;
        };
        let want = (WIDTH as u32, (last_y + last_h) as u32);
        match &self.layer {
            None => {
                let surface = self.compositor.create_surface(&self.qh);
                let layer =
                    self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("vesper-notify"), None);
                layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
                layer.set_margin(self.top_margin, 12, 0, 0);
                layer.set_exclusive_zone(-1);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.set_size(want.0, want.1);
                layer.commit();
                self.layer = Some(layer);
                self.size = want;
            }
            Some(layer) if want != self.size => {
                layer.set_size(want.0, want.1);
                layer.commit(); // the configure that follows redraws
                self.size = want;
            }
            Some(_) => self.redraw(),
        }
    }

    fn redraw(&mut self) {
        let Some(layer) = &self.layer else { return };
        if !self.configured || self.size.0 == 0 {
            return;
        }
        let (w, h) = self.size;
        let rows = self.popup_rows();
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            tracing::warn!("could not allocate a {w}x{h} buffer");
            return;
        };
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        for (card, &(y, ch)) in self.cards.iter().take(MAX_VISIBLE).zip(&rows) {
            let (radius, fill_a, border_w, border) = Self::bar_look(&self.config, &self.pal);
            c.rounded_rect(0, y, w as i32, ch, radius.min(ch as f32 / 2.0), alpha(self.pal.surface_container, fill_a));
            c.rounded_outline(0, y, w as i32, ch, radius.min(ch as f32 / 2.0), border_w, border);
            let accent = match card.n.urgency {
                0 => self.pal.outline,
                1 => self.pal.primary,
                _ => self.pal.error,
            };
            c.rounded_rect(0, y + 14, 4, CARD_H - 28, 2.0, accent);
            hits.push(((0, y, w as i32, CARD_H), Hit::Dismiss(card.n.id)));

            let ix = PAD + 6;
            let iy = y + (CARD_H - ICON) / 2;
            draw_icon(&mut c, &self.text, card.icon.as_ref(), &card.n, ix, iy);

            let tx = ix + ICON + 12;
            let avail = w as i32 - tx - PAD;
            let summary = self.text.fit(&card.n.summary, avail);
            self.text.draw(&mut c, tx, y + 24, &summary, self.pal.on_surface);
            let body = strip_markup(&card.n.body);
            for (line_no, line) in wrap(&self.small, &body, avail, 2).iter().enumerate() {
                self.small.draw(&mut c, tx, y + 46 + line_no as i32 * 17, line, self.pal.on_surface_variant);
            }

            // buttons the sender asked for
            if !card.n.actions.is_empty() {
                let mut x = PAD + 6;
                let by = y + CARD_H - 4;
                for (key, label) in card.n.actions.iter().take(4) {
                    let label = self.small.fit(label, 110);
                    let bw = self.small.width(&label) + 22;
                    c.rounded_rect(x, by, bw, ACT_H - 8, 9.0, self.pal.primary_container);
                    self.small.draw_centered(&mut c, x + bw / 2, by + (ACT_H - 8) / 2, &label, self.pal.on_primary_container);
                    hits.push(((x, by, bw, ACT_H - 8), Hit::Action(card.n.id, key.clone())));
                    x += bw + 8;
                }
            }
        }
        if self.cards.len() > MAX_VISIBLE {
            // a hint that more are waiting is enough; they show as the others go
            c.circle(w as i32 - 10, 8, 3, self.pal.primary);
        }
        self.hits = hits;

        layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        let (x, y) = (x as i32, y as i32);
        // buttons are listed after their card, so look from the end
        let hit = self.hits.iter().rev().find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(_, h)| h.clone());
        match hit {
            Some(Hit::Action(id, key)) => {
                emit_action(&self.dbus, id, &key);
                self.remove(id, Reason::Dismissed);
            }
            Some(Hit::Dismiss(id)) => self.remove(id, Reason::Dismissed),
            _ => {}
        }
    }

    // ------------------------------------------------------------ notification centre

    fn toggle_center(&mut self) {
        if self.center.is_some() {
            self.center = None;
            self.relayout();
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("vesper-notify-center"), None);
        layer.set_anchor(Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM);
        layer.set_margin(self.top_margin, 12, 12, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.set_size(CENTER_W as u32, 0);
        layer.commit();
        self.center = Some(Center { layer, configured: false, size: (CENTER_W as u32, 0), scroll: 0, hits: Vec::new() });
        self.relayout();
    }

    fn center_redraw(&mut self) {
        let Some(center) = &self.center else { return };
        if !center.configured || center.size.1 == 0 {
            return;
        }
        let (w, h) = center.size;
        let dnd_on = self.dnd_now();
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas::new(canvas, w, h);
        let (wi, hi) = (w as i32, h as i32);
        c.clear([0, 0, 0, 0]);
        let (radius, fill_a, border_w, border) = Self::bar_look(&self.config, &self.pal);
        c.rounded_rect(0, 0, wi, hi, radius, alpha(self.pal.surface, fill_a));
        c.rounded_outline(0, 0, wi, hi, radius, border_w, border);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();

        // header
        self.text.draw(&mut c, 20, 34, "Notifications", self.pal.on_surface);
        let close = (wi - 20 - 28, 14, 28, 28);
        c.circle(close.0 + 14, close.1 + 14, 14, self.pal.surface_container_high);
        self.text.draw_centered(&mut c, close.0 + 14, close.1 + 14, "✕", self.pal.on_surface_variant);
        hits.push((close, Hit::Close));

        let chip = |c: &mut Canvas, x: i32, y: i32, label: &str, on: bool, pal: &Palette, small: &Text| -> (i32, i32, i32, i32) {
            let bw = small.width(label) + 24;
            let (bg, fg) = if on { (pal.primary, pal.on_primary) } else { (pal.surface_container_high, pal.on_surface) };
            c.rounded_rect(x, y, bw, 30, 15.0, bg);
            small.draw_centered(c, x + bw / 2, y + 15, label, fg);
            (x, y, bw, 30)
        };
        let dnd = chip(&mut c, 20, 54, if dnd_on { "Do not disturb: on" } else { "Do not disturb" }, dnd_on, &self.pal, &self.small);
        hits.push((dnd, Hit::Dnd));
        let clear = chip(&mut c, dnd.0 + dnd.2 + 8, 54, "Clear all", false, &self.pal, &self.small);
        hits.push((clear, Hit::Clear));

        // list
        let top = 100;
        let item_h = 76;
        let total = self.history.len() as i32 * (item_h + GAP);
        let max_scroll = (total - (hi - top - 10)).max(0);
        let scroll = center.scroll.clamp(0, max_scroll);
        if self.history.is_empty() {
            self.small.draw_centered(&mut c, wi / 2, top + 60, "No notifications", self.pal.on_surface_variant);
        }
        for (i, past) in self.history.iter().enumerate() {
            let y = top + i as i32 * (item_h + GAP) - scroll;
            if y + item_h < top || y > hi {
                continue;
            }
            c.rounded_rect(16, y, wi - 32, item_h, 12.0, alpha(self.pal.surface_container, 0xf2));
            let accent = match past.n.urgency {
                0 => self.pal.outline,
                1 => self.pal.primary,
                _ => self.pal.error,
            };
            c.rounded_rect(16, y + 14, 3, item_h - 28, 1.5, accent);
            draw_icon(&mut c, &self.text, None, &past.n, 30, y + (item_h - ICON) / 2);
            let tx = 30 + ICON + 12;
            let avail = wi - tx - 24;
            let when = ago(past.at.elapsed().as_secs());
            let wwidth = self.small.width(&when);
            let summary = self.text.fit(&past.n.summary, avail - wwidth - 8);
            self.text.draw(&mut c, tx, y + 26, &summary, self.pal.on_surface);
            self.small.draw(&mut c, wi - 24 - wwidth, y + 26, &when, self.pal.on_surface_variant);
            let body = strip_markup(&past.n.body);
            for (n, line) in wrap(&self.small, &body, avail, 2).iter().enumerate() {
                self.small.draw(&mut c, tx, y + 48 + n as i32 * 16, line, self.pal.on_surface_variant);
            }
            hits.push(((16, y.max(top), wi - 32, item_h), Hit::Forget(past.key)));
        }
        // keep the header clean while scrolling
        let center = self.center.as_mut().unwrap();
        center.scroll = scroll;
        center.hits = hits;
        let layer = center.layer.clone();
        layer.wl_surface().damage_buffer(0, 0, wi, hi);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
        }
    }

    fn center_click(&mut self, x: f64, y: f64) {
        let (x, y) = (x as i32, y as i32);
        let Some(center) = &self.center else { return };
        let hit = center.hits.iter().rev().find(|((rx, ry, rw, rh), _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh).map(|(_, h)| h.clone());
        match hit {
            Some(Hit::Close) => {
                self.center = None;
                self.relayout();
            }
            Some(Hit::Clear) => {
                self.history.clear();
                self.center_redraw();
            }
            Some(Hit::Dnd) => {
                self.dnd = !self.dnd;
                let _ = vesper_config::Config::set("notifications.do_not_disturb", self.dnd.into());
                self.center_redraw();
            }
            Some(Hit::Forget(key)) => {
                self.history.retain(|p| p.key != key);
                self.center_redraw();
            }
            _ => {}
        }
    }

    fn center_redraw_if_stale(&mut self) {
        if self.center.is_some() && self.last_center_draw.elapsed() >= Duration::from_secs(30) {
            self.last_center_draw = Instant::now();
            self.center_redraw();
        }
    }

    fn remember(&mut self, n: &Notification) {
        self.history_key += 1;
        if n.replaces {
            self.history.retain(|p| p.n.id != n.id);
        }
        self.history.insert(0, Past { key: self.history_key, n: n.clone(), at: Instant::now() });
        self.history.truncate(self.config.notifications.history_max.max(1));
        self.center_redraw();
    }

    /// Tell the bar how many notifications are waiting (a tiny file in the runtime dir).
    fn publish_state(&mut self) {
        let text = format!("{} {}", self.history.len(), u8::from(self.dnd_now()));
        if text != self.published {
            let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
            let _ = std::fs::write(format!("{dir}/vesper-notify.state"), &text);
            self.published = text;
        }
    }

    /// Pick up edits to the config file (do-not-disturb, history size, default timeout, bar height).
    fn refresh_config(&mut self) {
        let stamp = vesper_config::Config::stamp();
        if stamp != self.config_stamp {
            self.config_stamp = stamp;
            self.config = vesper_config::Config::load();
            self.dnd = self.config.notifications.do_not_disturb;
            self.top_margin = self.config.bar.height as i32 + 8;
            self.center_redraw();
        }
    }
}

fn draw_icon(c: &mut Canvas, text: &Text, img: Option<&Image>, n: &Notification, ix: i32, iy: i32) {
    match img {
        Some(img) => c.image(ix, iy, img),
        None => {
            let name = if n.app_name.is_empty() { &n.summary } else { &n.app_name };
            c.rounded_rect(ix, iy, ICON, ICON, ICON as f32 * 0.26, tile_color(name));
            let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_else(|| "!".into());
            text.draw_centered(c, ix + ICON / 2, iy + ICON / 2, &initial, TILE_TEXT);
        }
    }
}

/// "just now", "5m ago", "2h ago", "3d ago".
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Greedy word wrap into at most `max_lines` lines; the last line is truncated with an ellipsis.
fn wrap(text: &Text, s: &str, width: i32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let words: Vec<&str> = s.split_whitespace().collect();
    for (wi, word) in words.iter().enumerate() {
        let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if text.width(&candidate) <= width {
            cur = candidate;
            continue;
        }
        if lines.len() + 1 >= max_lines {
            // out of room: fit what is left (including this word) on the last line
            let rest = std::iter::once(cur.as_str()).chain(words[wi..].iter().copied()).collect::<Vec<_>>().join(" ");
            lines.push(text.fit(rest.trim(), width));
            return lines;
        }
        lines.push(std::mem::take(&mut cur));
        cur = text.fit(word, width);
    }
    if !cur.is_empty() {
        lines.push(text.fit(&cur, width));
    }
    lines
}

/// A stable, pleasant colour for an app name.
fn tile_color(name: &str) -> Rgba {
    let hash = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let hue = (hash % 360) as f32;
    let (s, l) = (0.45f32, 0.42f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8, 0xff]
}

impl LayerShellHandler for Notify {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface) {
        if self.center.as_ref().is_some_and(|c| c.layer.wl_surface() == l.wl_surface()) {
            self.center = None;
            return;
        }
        self.layer = None;
        self.configured = false;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, l: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let Some(center) = self.center.as_mut().filter(|ce| ce.layer.wl_surface() == l.wl_surface()) {
            if c.new_size.0 > 0 && c.new_size.1 > 0 {
                center.size = c.new_size;
            }
            center.configured = true;
            self.center_redraw();
            return;
        }
        if let (Some(w), Some(h)) = (NonZeroU32::new(c.new_size.0), NonZeroU32::new(c.new_size.1)) {
            self.size = (w.get(), h.get());
        }
        self.configured = true;
        self.redraw();
    }
}

impl PointerHandler for Notify {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            let ours = self.layer.as_ref().is_some_and(|l| &e.surface == l.wl_surface());
            let centre = self.center.as_ref().is_some_and(|c| &e.surface == c.layer.wl_surface());
            match e.kind {
                PointerEventKind::Press { button: BTN_LEFT, .. } if ours => self.click(e.position.0, e.position.1),
                PointerEventKind::Press { button: BTN_LEFT, .. } if centre => self.center_click(e.position.0, e.position.1),
                PointerEventKind::Axis { vertical, .. } if centre => {
                    if let Some(c) = self.center.as_mut() {
                        c.scroll += (vertical.absolute * 1.0) as i32;
                    }
                    self.center_redraw();
                }
                _ => {}
            }
        }
    }
}

impl SeatHandler for Notify {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
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

impl CompositorHandler for Notify {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Notify {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Notify {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Notify);
delegate_output!(Notify);
delegate_shm!(Notify);
delegate_seat!(Notify);
delegate_pointer!(Notify);
delegate_keyboard!(Notify);
delegate_layer!(Notify);
delegate_registry!(Notify);

impl ProvidesRegistryState for Notify {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl KeyboardHandler for Notify {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        if event.keysym == Keysym::Escape {
            self.center = None;
            self.relayout();
        }
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: smithay_client_toolkit::seat::keyboard::RawModifiers, _: u32) {}
}

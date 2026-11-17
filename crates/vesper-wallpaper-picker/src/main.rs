//! vesper-wallpaper-picker: the wallpaper changer (Mod+W).
//!
//! The screen dims and your wallpapers form a centred carousel: the selected one is large, its neighbours are narrower
//! strips, and the rest thin slivers. Left/Right (or the wheel) slide along it, Enter or a click applies, typing filters
//! by name, and the colour chips filter by dominant colour. Tab cycles how the image fits; Esc closes.

mod theme_ui;
mod thumbs;

use std::{cell::RefCell, num::NonZeroU32, path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
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
use thumbs::{ColorTag, THUMB_H, THUMB_W};
use theme_ui::{Fonts, Outcome, Status, ThemeUi};
use vesper_config::{
    theme::{self, alpha, with_alpha, Palette},
    WallpaperChoice,
};
use vesper_gfx::{Canvas, Image, Rgba, Text};

const MODES: [&str; 4] = ["fill", "fit", "stretch", "center"];
const BTN_LEFT: u32 = 0x110;
const CHIPS: [(ColorTag, Rgba); 5] = [
    (ColorTag::Red, [0xe8, 0x45, 0x4f, 0xff]),
    (ColorTag::Orange, [0xf5, 0x9a, 0x3b, 0xff]),
    (ColorTag::Green, [0x5f, 0xc4, 0x5f, 0xff]),
    (ColorTag::Blue, [0x3d, 0x7b, 0xe8, 0xff]),
    (ColorTag::Gray, [0x8a, 0x8f, 0x98, 0xff]),
];

/// Sizes of everything, derived from the screen height so the carousel looks the same on any display.
#[derive(Debug, Clone, Copy)]
struct Geo {
    card_h: i32,
    sel_w: i32,
    near_w: i32,
    far_w: i32,
    gap: i32,
}

fn geometry(screen_h: i32) -> Geo {
    let card_h = ((screen_h as f32 * 0.27) as i32).clamp(200, 380);
    Geo {
        card_h,
        sel_w: card_h * 16 / 9,
        near_w: card_h * 39 / 100,
        far_w: (card_h * 9 / 100).max(18),
        gap: ((card_h as f32 * 0.032) as i32).max(6),
    }
}

/// Width of a card `d` places away from the (fractional) selection: wide at 0, medium at 1, thin from 2 on.
fn card_width(d: f32, g: &Geo) -> i32 {
    let lerp = |a: i32, b: i32, t: f32| a as f32 + (b - a) as f32 * t;
    let w = if d <= 1.0 { lerp(g.sel_w, g.near_w, d) } else if d <= 2.0 { lerp(g.near_w, g.far_w, d - 1.0) } else { g.far_w as f32 };
    w.round() as i32
}

/// Horizontal position and width of every card for a selection at `pos`, with the selection centred on the screen.
fn layout(count: usize, pos: f32, screen_w: i32, g: &Geo) -> Vec<(i32, i32)> {
    if count == 0 {
        return Vec::new();
    }
    let widths: Vec<i32> = (0..count).map(|i| card_width((i as f32 - pos).abs(), g)).collect();
    let mut xs = Vec::with_capacity(count);
    let mut x = 0;
    for w in &widths {
        xs.push(x);
        x += w + g.gap;
    }
    let center = |i: usize| xs[i] as f32 + widths[i] as f32 / 2.0;
    let i0 = (pos.floor().max(0.0) as usize).min(count - 1);
    let i1 = (i0 + 1).min(count - 1);
    let frac = (pos - i0 as f32).clamp(0.0, 1.0);
    let c = center(i0) + (center(i1) - center(i0)) * frac;
    let offset = (screen_w as f32 / 2.0 - c).round() as i32;
    xs.iter().zip(&widths).map(|(x, w)| (x + offset, *w)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Screen {
    Wallpapers,
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    /// The "Theme" button.
    Theme,
    /// A card, by position in the visible list.
    Card(usize),
    /// A colour chip (None = All).
    Chip(Option<ColorTag>),
}

struct Item {
    path: PathBuf,
    /// Shown under the card: upper case, underscores as spaces.
    label: String,
    thumb: Option<Image>,
    tag: Option<ColorTag>,
}

struct Picker {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    configured: bool,
    exit: bool,
    size: (i32, i32),

    items: Vec<Item>,
    /// Indices into `items` that pass the current filters.
    visible: Vec<usize>,
    /// The selected card, as a position in `visible`.
    sel: usize,
    /// The animated selection: eases towards `sel`.
    pos: f32,
    animating: bool,
    query: String,
    color: Option<ColorTag>,
    /// The wallpaper in use now (marked on its card) and how it fits the screen.
    current: String,
    mode: usize,
    pal: Palette,
    screen: Screen,
    theme: ThemeUi,
    /// matugen runs in the background; results arrive on this channel.
    gen_tx: channel::Sender<Result<String, String>>,
    gen_busy: bool,
    gen_dirty: bool,
    hits: Vec<([i32; 4], Hit)>,

    title: Text,
    name: Text,
    small: Text,
    body: Text,
}

thread_local! {
    static LOOP: RefCell<Option<LoopHandle<'static, Picker>>> = const { RefCell::new(None) };
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| p.into()),
        None => p.into(),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();
    let files = thumbs::scan(&expand_home(&config.wallpaper.dir));

    // What is showing now: the saved choice, else the config.
    let (current, mode) = match WallpaperChoice::load() {
        Some(c) if !c.path.is_empty() => (c.path, c.mode),
        _ => (config.wallpaper.path.clone(), config.wallpaper.mode.clone()),
    };
    let current_path = expand_home(&current);
    let mode = MODES.iter().position(|m| *m == mode).unwrap_or(0);

    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    // Cover the whole output: the backdrop dims everything behind the carousel.
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-wallpaper-picker"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let font = Text::new(&config.bar.font, 15.0);
    let (gen_tx, gen_rx) = channel::channel::<Result<String, String>>();
    let items: Vec<Item> = files
        .iter()
        .map(|p| Item {
            path: p.clone(),
            label: p.file_stem().map(|n| n.to_string_lossy().replace(['_', '-'], " ").to_uppercase()).unwrap_or_default(),
            thumb: None,
            tag: None,
        })
        .collect();
    let start_sel = files.iter().position(|p| *p == current_path).unwrap_or(0);
    let mut app = Picker {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(1920 * 1200 * 4, &shm)?,
        shm,
        layer,
        keyboard: None,
        pointer: None,
        configured: false,
        exit: false,
        size: (0, 0),
        visible: (0..items.len()).collect(),
        items,
        sel: start_sel,
        pos: start_sel as f32,
        animating: false,
        query: String::new(),
        color: None,
        current: current_path.to_string_lossy().into_owned(),
        mode,
        pal: Palette::load(),
        screen: if std::env::args().any(|a| a == "--theme") { Screen::Theme } else { Screen::Wallpapers },
        theme: ThemeUi::new(),
        gen_tx,
        gen_busy: false,
        gen_dirty: false,
        hits: Vec::new(),
        name: font.resized(26.0),
        body: font.resized(16.0),
        small: font.resized(14.0),
        title: font,
    };

    let mut event_loop: EventLoop<Picker> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    LOOP.with(|l| *l.borrow_mut() = Some(handle.clone()));
    WaylandSource::new(conn, queue).insert(handle.clone()).map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(gen_rx, |event, _, app| {
            if let channel::Event::Msg(result) = event {
                app.generation_done(result);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    // Thumbnails arrive from worker threads. Load the wallpaper in use first, so the selected card fills in quickly.
    let (tx, rx) = channel::channel::<(usize, Vec<u8>, ColorTag)>();
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg((i, rgba, tag)) = event {
                if let Some(item) = app.items.get_mut(i) {
                    item.thumb = Some(Image::from_rgba(THUMB_W as i32, THUMB_H as i32, &rgba, false));
                    item.tag = Some(tag);
                }
                if app.color.is_some() {
                    app.refilter();
                }
                app.redraw();
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    let n = files.len();
    let mut order = files.clone();
    order.rotate_left(start_sel.min(n));
    let (rtx, rrx) = channel::channel::<(usize, Vec<u8>, ColorTag)>();
    handle
        .insert_source(rrx, move |event, _, _| {
            if let channel::Event::Msg((i, rgba, tag)) = event {
                let _ = tx.send(((i + start_sel) % n.max(1), rgba, tag));
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    thumbs::spawn_loaders(order, 4, rtx);
    handle
        .insert_source(Timer::from_duration(Duration::from_millis(300)), |_, _, app: &mut Picker| {
            let p = Palette::load();
            if p != app.pal {
                app.pal = p;
                app.redraw();
            }
            TimeoutAction::ToDuration(Duration::from_millis(300))
        })
        .map_err(|e| anyhow!("{e}"))?;

    while !app.exit {
        event_loop.dispatch(None, &mut app)?;
    }
    Ok(())
}

impl Picker {
    /// Recompute which wallpapers pass the name and colour filters, keeping the same one selected if it still does.
    fn refilter(&mut self) {
        let keep = self.visible.get(self.sel).copied();
        let q = self.query.to_lowercase();
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                (q.is_empty() || it.label.to_lowercase().contains(&q))
                    && self.color.is_none_or(|c| it.tag == Some(c))
            })
            .map(|(i, _)| i)
            .collect();
        self.sel = keep.and_then(|k| self.visible.iter().position(|&v| v == k)).unwrap_or(0);
        self.start_anim();
    }

    fn start_anim(&mut self) {
        if self.animating {
            return;
        }
        self.animating = true;
        if let Some(handle) = LOOP.with(|l| l.borrow().clone()) {
            let _ = handle.insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, app| app.tick());
        }
    }

    /// One animation step: ease the displayed position towards the selection.
    fn tick(&mut self) -> TimeoutAction {
        let target = self.sel as f32;
        let d = target - self.pos;
        if d.abs() < 0.004 {
            self.pos = target;
            self.animating = false;
            self.redraw();
            return TimeoutAction::Drop;
        }
        self.pos += d * 0.28;
        self.redraw();
        TimeoutAction::ToDuration(Duration::from_millis(16))
    }

    fn step(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        self.sel = (self.sel as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.start_anim();
    }

    fn set_color(&mut self, color: Option<ColorTag>) {
        self.color = if self.color == color { None } else { color };
        self.refilter();
        self.redraw();
    }

    /// Cycle the colour chip: All, red, orange, ...
    fn step_color(&mut self, delta: isize) {
        let all: Vec<Option<ColorTag>> = std::iter::once(None).chain(CHIPS.iter().map(|(t, _)| Some(*t))).collect();
        let at = all.iter().position(|c| *c == self.color).unwrap_or(0) as isize;
        self.color = all[(at + delta).rem_euclid(all.len() as isize) as usize];
        self.refilter();
        self.redraw();
    }

    fn apply(&mut self) {
        let Some(&i) = self.visible.get(self.sel) else { return };
        let path = &self.items[i].path;
        let choice = WallpaperChoice { path: path.to_string_lossy().into_owned(), mode: MODES[self.mode].to_owned() };
        match choice.save() {
            Ok(()) => tracing::info!("wallpaper set to {}", path.display()),
            Err(err) => tracing::error!("could not save the wallpaper choice: {err}"),
        }
        self.exit = true;
    }

    fn cycle_mode(&mut self) {
        self.mode = (self.mode + 1) % MODES.len();
        // If a wallpaper is already showing, let you see the new fit straight away.
        if !self.current.is_empty() {
            let _ = WallpaperChoice { path: self.current.clone(), mode: MODES[self.mode].to_owned() }.save();
        }
        self.redraw();
    }

    fn key(&mut self, event: KeyEvent) {
        if self.screen == Screen::Theme {
            let outcome = self.theme.key(&event);
            return self.apply_outcome(outcome);
        }
        match event.keysym {
            Keysym::Escape => self.exit = true,
            Keysym::F2 => self.open_theme(),
            Keysym::Return | Keysym::KP_Enter => self.apply(),
            Keysym::Left => self.step(-1),
            Keysym::Right => self.step(1),
            Keysym::Home => self.step(-(self.sel as isize)),
            Keysym::End => self.step(self.visible.len() as isize),
            Keysym::Up => self.step_color(-1),
            Keysym::Down => self.step_color(1),
            Keysym::Tab => self.cycle_mode(),
            Keysym::BackSpace => {
                if self.query.pop().is_some() {
                    self.refilter();
                    self.redraw();
                }
            }
            _ => {
                if let Some(s) = event.utf8 {
                    let printable: String = s.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() && self.query.len() < 40 {
                        self.query.push_str(&printable);
                        self.refilter();
                        self.redraw();
                    }
                }
            }
        }
    }

    fn redraw(&mut self) {
        if !self.configured || self.size.0 == 0 {
            return;
        }
        let pal = self.pal.clone();
        let (w, h) = self.size;
        let g = geometry(h);
        let s = g.card_h as f32 / 323.0; // sizes below were designed against a 323px card
        let at = |v: f32| (v * s).round() as i32;
        let (cx, cy) = (w / 2, h / 2);
        let (chips_y, title_y, card_top, line_y, name_y, hints_y) =
            (cy - at(255.0), cy - at(213.0), cy - at(175.0), cy + at(170.0), cy + at(205.0), cy + at(249.0));

        let rects = layout(self.visible.len(), self.pos, w, &g);
        let counts = |tag: ColorTag| self.items.iter().filter(|it| it.tag == Some(tag)).count();
        let chip_counts: Vec<usize> = CHIPS.iter().map(|(t, _)| counts(*t)).collect();
        let (accent, color, mode, query, sel_pos) = (pal.primary, self.color, MODES[self.mode], self.query.clone(), self.sel);
        let total_visible = self.visible.len();
        let sel_label = self.visible.get(self.sel).map(|&i| self.items[i].label.clone());

        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas::new(canvas, w as u32, h as u32);
        c.clear(alpha(pal.scrim, 0x9e));
        if self.screen == Screen::Theme {
            let fonts = Fonts { title: &self.title, body: &self.body, small: &self.small };
            self.theme.draw(&mut c, w, h, &fonts, &pal);
            self.layer.wl_surface().damage_buffer(0, 0, w, h);
            if buffer.attach_to(self.layer.wl_surface()).is_ok() {
                self.layer.commit();
            }
            return;
        }
        let mut hits: Vec<([i32; 4], Hit)> = Vec::new();

        // --- colour chips: All, then red, orange, green, blue, gray
        let (all_w, chip_h, r) = (at(44.0).max(40), at(26.0).max(24), at(12.0).max(10));
        let chip_gap = at(14.0);
        let total_w = all_w + CHIPS.len() as i32 * (2 * r + chip_gap);
        let mut x = cx - total_w / 2;
        let all_active = color.is_none();
        c.rounded_rect(x, chips_y - chip_h / 2, all_w, chip_h, chip_h as f32 / 2.0, with_alpha(accent, if all_active { 0.22 } else { 0.0 }));
        c.rounded_outline(x, chips_y - chip_h / 2, all_w, chip_h, chip_h as f32 / 2.0, 1.5, if all_active { accent } else { with_alpha(pal.on_surface, 0.28) });
        self.small.draw_centered(&mut c, x + all_w / 2, chips_y, "All", if all_active { pal.on_surface } else { pal.on_surface_variant });
        hits.push(([x, chips_y - chip_h / 2, all_w, chip_h], Hit::Chip(None)));
        x += all_w + chip_gap;
        for (k, (tag, rgb)) in CHIPS.iter().enumerate() {
            let active = color == Some(*tag);
            let dim = chip_counts[k] == 0 && !active;
            c.circle(x + r, chips_y, r, if dim { with_alpha(*rgb, 0.6) } else { *rgb });
            if active {
                c.rounded_outline(x - 4, chips_y - r - 4, 2 * r + 8, 2 * r + 8, (r + 4) as f32, 2.0, accent);
            }
            hits.push(([x - 4, chips_y - r - 4, 2 * r + 8, 2 * r + 8], Hit::Chip(Some(*tag))));
            x += 2 * r + chip_gap;
        }

        // --- cards
        if total_visible == 0 {
            self.name.draw_centered_spaced(&mut c, cx, card_top + g.card_h / 2, "NO WALLPAPERS MATCH", pal.on_surface_variant, 2);
        }
        let radius = 10.0 * s.max(0.7);
        for (i, &(cx0, cw)) in rects.iter().enumerate() {
            if cx0 + cw < 0 || cx0 > w {
                continue;
            }
            let sw = (1.0 - (i as f32 - self.pos).abs()).clamp(0.0, 1.0);
            let item = &self.items[self.visible[i]];
            match &item.thumb {
                Some(img) => c.draw_cover(img, cx0, card_top, cw, g.card_h, radius, 0.55 + 0.45 * sw),
                None => c.rounded_rect(cx0, card_top, cw, g.card_h, radius, pal.surface_container_high),
            }
            let ring = if sw > 0.0 {
                let a = with_alpha(pal.on_surface, 0.26);
                [
                    (a[0] as f32 + (accent[0] as f32 - a[0] as f32) * sw) as u8,
                    (a[1] as f32 + (accent[1] as f32 - a[1] as f32) * sw) as u8,
                    (a[2] as f32 + (accent[2] as f32 - a[2] as f32) * sw) as u8,
                    (a[3] as f32 + (255.0 - a[3] as f32) * sw) as u8,
                ]
            } else {
                with_alpha(pal.on_surface, 0.26)
            };
            c.rounded_outline(cx0, card_top, cw, g.card_h, radius, if sw > 0.5 { 2.0 } else { 1.5 }, ring);
            if item.path.to_string_lossy() == self.current && cw > 60 {
                c.circle(cx0 + cw - 16, card_top + 16, 7, accent);
                c.circle(cx0 + cw - 16, card_top + 16, 2, pal.on_surface);
            }
            hits.push(([cx0, card_top, cw, g.card_h], Hit::Card(i)));
        }

        // --- header: title over the selected card, counter at its right edge
        let sel_rect = rects.get(sel_pos).copied();
        let title_cx = sel_rect.map_or(cx, |(x0, w0)| x0 + w0 / 2);
        self.title.draw_centered_spaced(&mut c, title_cx, title_y, "WALLPAPER", [0xe6, 0xe6, 0xf0, 0xff], 6);
        if let Some((x0, w0)) = sel_rect {
            let counter = format!("{} / {}", sel_pos + 1, total_visible);
            self.title.draw(&mut c, x0 + w0 - self.title.width(&counter), title_y, &counter, pal.on_surface_variant);
            if !query.is_empty() {
                self.title.draw(&mut c, x0, title_y, &format!("filter: {query}"), accent);
            }
        }

        // --- under the card: a soft line, the name, the hints
        c.glow_line(title_cx, line_y, at(125.0), 2, accent);
        if let Some(label) = sel_label {
            self.name.draw_centered_spaced(&mut c, title_cx, name_y, &label, pal.on_surface, 3);
        }
        let hints = [
            ("← →", "scroll navigate"),
            ("Enter", "apply"),
            ("Esc", "cancel"),
            ("type", "to filter"),
            ("click", "a colour to filter by it"),
            ("Tab", &format!("fit: {mode}")),
            ("F2", "theme"),
        ];
        let sep = at(34.0);
        let widths: Vec<i32> = hints.iter().map(|(k, d)| self.small.width(k) + 10 + self.small.width(d)).collect();
        let mut hx = cx - (widths.iter().sum::<i32>() + sep * (hints.len() as i32 - 1)) / 2;
        for ((k, d), wd) in hints.iter().zip(&widths) {
            self.small.draw(&mut c, hx, hints_y, k, pal.on_surface);
            self.small.draw(&mut c, hx + self.small.width(k) + 10, hints_y, d, pal.on_surface_variant);
            hx += wd + sep;
        }

        // the Theme button, top right
        let (bw, bh) = (104, 34);
        let bx = w - 32 - bw;
        c.rounded_rect(bx, 32, bw, bh, 17.0, with_alpha(pal.on_surface, 0.10));
        c.rounded_outline(bx, 32, bw, bh, 17.0, 1.0, with_alpha(pal.on_surface, 0.28));
        self.small.draw_centered(&mut c, bx + bw / 2, 32 + bh / 2, "◐  Theme", pal.on_surface);
        hits.push(([bx, 32, bw, bh], Hit::Theme));

        self.hits = hits;
        self.layer.wl_surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(self.layer.wl_surface()).is_ok() {
            self.layer.commit();
        }
    }

    fn click(&mut self, x: f64, y: f64) {
        if self.screen == Screen::Theme {
            let outcome = self.theme.click(x as i32, y as i32);
            return self.apply_outcome(outcome);
        }
        let (x, y) = (x as i32, y as i32);
        // later hits are drawn on top; chips are added first, so search from the end for cards
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|([rx, ry, rw, rh], _)| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
            .map(|(_, h)| *h);
        match hit {
            Some(Hit::Card(i)) if i == self.sel => self.apply(),
            Some(Hit::Card(i)) => {
                self.sel = i;
                self.start_anim();
            }
            Some(Hit::Chip(tag)) => self.set_color(tag),
            Some(Hit::Theme) => self.open_theme(),
            None => self.exit = true, // clicked the backdrop
        }
    }
}

impl LayerShellHandler for Picker {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(c.new_size.0), NonZeroU32::new(c.new_size.1)) {
            self.size = (w.get() as i32, h.get() as i32);
        }
        self.configured = true;
        self.redraw();
    }
}

impl PointerHandler for Picker {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            if &e.surface != self.layer.wl_surface() {
                continue;
            }
            match e.kind {
                PointerEventKind::Press { button: BTN_LEFT, .. } => self.click(e.position.0, e.position.1),
                PointerEventKind::Axis { vertical, horizontal, .. } => {
                    let amount = if vertical.absolute != 0.0 || vertical.discrete != 0 {
                        (vertical.absolute, vertical.discrete)
                    } else {
                        (horizontal.absolute, horizontal.discrete)
                    };
                    if amount.0 > 0.0 || amount.1 > 0 {
                        self.step(1);
                    } else if amount.0 < 0.0 || amount.1 < 0 {
                        self.step(-1);
                    }
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for Picker {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        if self.configured {
            self.exit = true;
        }
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl SeatHandler for Picker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        match cap {
            Capability::Keyboard if self.keyboard.is_none() => {
                let handle = LOOP.with(|l| l.borrow().clone());
                self.keyboard = match handle {
                    Some(h) => self
                        .seat_state
                        .get_keyboard_with_repeat(qh, &seat, None, h, Box::new(|p: &mut Picker, _, ev| p.key(ev)))
                        .ok(),
                    None => self.seat_state.get_keyboard(qh, &seat, None).ok(),
                };
            }
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

impl CompositorHandler for Picker {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Picker);
delegate_output!(Picker);
delegate_shm!(Picker);
delegate_seat!(Picker);
delegate_pointer!(Picker);
delegate_keyboard!(Picker);
delegate_layer!(Picker);
delegate_registry!(Picker);

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_card_is_centred_and_widest() {
        let g = geometry(1200);
        for sel in [0usize, 3, 9] {
            let r = layout(10, sel as f32, 1920, &g);
            let (x, w) = r[sel];
            assert_eq!(w, g.sel_w);
            assert!((x + w / 2 - 960).abs() <= 1, "selected card centred, got {}", x + w / 2);
            // widths shrink with distance
            if sel + 1 < 10 {
                assert_eq!(r[sel + 1].1, g.near_w);
            }
            if sel + 2 < 10 {
                assert_eq!(r[sel + 2].1, g.far_w);
            }
        }
    }

    #[test]
    fn cards_never_overlap_and_move_smoothly() {
        let g = geometry(1080);
        let mut last_center = None;
        for step in 0..=20 {
            let pos = 2.0 + step as f32 * 0.05;
            let r = layout(8, pos, 1920, &g);
            for pair in r.windows(2) {
                assert!(pair[0].0 + pair[0].1 + g.gap <= pair[1].0 + 1, "overlap at pos {pos}");
            }
            // the point on screen where the selection sits stays at the centre the whole time
            let i = pos.floor() as usize;
            let t = pos - i as f32;
            let c0 = r[i].0 as f32 + r[i].1 as f32 / 2.0;
            let c1 = r[i + 1].0 as f32 + r[i + 1].1 as f32 / 2.0;
            let c = c0 + (c1 - c0) * t;
            assert!((c - 960.0).abs() < 2.0, "interpolated centre {c} at pos {pos}");
            last_center = Some(c);
        }
        assert!(last_center.is_some());
    }

    #[test]
    fn empty_and_single() {
        let g = geometry(1080);
        assert!(layout(0, 0.0, 1920, &g).is_empty());
        let one = layout(1, 0.0, 1920, &g);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].1, g.sel_w);
    }
}

impl Picker {
    fn open_theme(&mut self) {
        self.theme = ThemeUi::new();
        self.screen = Screen::Theme;
        self.redraw();
    }

    fn apply_outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::None => {}
            Outcome::Redraw => self.redraw(),
            Outcome::Back => {
                self.screen = Screen::Wallpapers;
                self.redraw();
            }
            Outcome::Regenerate => {
                self.regenerate();
                self.redraw();
            }
        }
    }

    /// Run matugen for the wallpaper in use, in the background. Changes made while it runs trigger one more run after.
    fn regenerate(&mut self) {
        if self.gen_busy {
            self.gen_dirty = true;
            return;
        }
        if self.current.is_empty() {
            self.theme.status = Status::Err("Pick a wallpaper first: close this and press Enter on one".into());
            return;
        }
        self.gen_busy = true;
        self.theme.status = Status::Working;
        let settings = self.theme.settings.clone();
        let image = theme::expand_home(&self.current);
        let tx = self.gen_tx.clone();
        std::thread::spawn(move || {
            let result = theme::regenerate(&image, &settings).map(|()| {
                if settings.enabled {
                    format!(
                        "Colours generated from {}  ·  {}  ·  {}",
                        image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        settings.mode,
                        theme_ui::scheme_label(&settings.scheme)
                    )
                } else {
                    "Theming is off: using Vesper's own colours".to_owned()
                }
            });
            let _ = tx.send(result);
        });
    }

    fn generation_done(&mut self, result: Result<String, String>) {
        self.gen_busy = false;
        self.theme.generation_finished(result);
        // the screen you are looking at wears the new colours straight away
        self.pal = Palette::load();
        if self.gen_dirty {
            self.gen_dirty = false;
            self.regenerate();
        }
        self.redraw();
    }
}

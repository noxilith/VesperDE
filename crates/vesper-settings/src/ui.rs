//! Layout, drawing and hit-testing of the settings window. Everything is laid out in logical pixels and multiplied by the scale.

use vesper_config::theme::{alpha, Palette};
use vesper_gfx::{Canvas, Image, Text};

use crate::model::{Act, Ctl, Page, Val, PAGES};

pub type Rect = (i32, i32, i32, i32);

#[derive(Clone, Debug)]
pub enum Hit {
    Nav(usize),
    Toggle(String, bool),
    Pick(String, Val),
    Step { path: String, cur: f64, delta: f64, min: f64, max: f64, int: bool },
    Edit { path: String, initial: String, numeric: Option<(f64, f64, bool)> },
    Slider { path: String, min: f64, max: f64, int: bool, pad: i32 },
    Drop { path: String, options: Vec<(Val, String)>, cur: Val, anchor: Rect },
    Tab(usize),
    Act(Act),
    CloseDrop,
}

pub struct Fonts {
    pub base: Text,
    pub small: Text,
    pub title: Text,
    pub icon: Text,
}

impl Fonts {
    pub fn new(family: &str, k: f32) -> Self {
        let base = Text::new(family, 15.0 * k);
        Self { small: base.resized(13.0 * k), title: base.resized(28.0 * k), icon: base.resized(20.0 * k), base }
    }

    pub fn rescale(&self, k: f32) -> Self {
        Self { base: self.base.resized(15.0 * k), small: self.base.resized(13.0 * k), title: self.base.resized(28.0 * k), icon: self.base.resized(20.0 * k) }
    }
}

pub struct EditState {
    pub path: String,
    pub text: String,
    pub numeric: Option<(f64, f64, bool)>,
}

pub struct DropState {
    pub path: String,
    pub options: Vec<(Val, String)>,
    pub cur: Val,
    pub anchor: Rect,
}

pub struct Ui<'a> {
    pub pal: &'a Palette,
    pub fonts: &'a Fonts,
    pub k: f32,
    pub w: i32,
    pub h: i32,
    pub page: &'a Page,
    pub nav: usize,
    pub tab: usize,
    pub scroll: i32,
    pub edit: Option<&'a EditState>,
    pub drop: Option<&'a DropState>,
    pub status: &'a str,
    pub pointer: (f64, f64),
}

pub struct Drawn {
    pub hits: Vec<(Rect, Hit)>,
    pub content_height: i32,
}

fn inside(r: &Rect, p: (f64, f64)) -> bool {
    p.0 >= r.0 as f64 && p.0 < (r.0 + r.2) as f64 && p.1 >= r.1 as f64 && p.1 < (r.1 + r.3) as f64
}

pub fn hit_at(hits: &[(Rect, Hit)], p: (f64, f64)) -> Option<usize> {
    hits.iter().rposition(|(r, _)| inside(r, p))
}

impl Ui<'_> {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.k).round() as i32
    }

    pub fn draw(&self, c: &mut Canvas) -> Drawn {
        let (p, f) = (self.pal, self.fonts);
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        c.clear(p.surface);

        let compact = self.w < self.px(720);
        let side_w = self.px(if compact { 64 } else { 244 });
        let pad = self.px(if compact { 20 } else { 40 });
        let cw = (self.w - side_w - pad * 2).clamp(self.px(320), self.px(900));
        let x0 = side_w + pad;
        let mut y = self.px(36) - self.scroll;

        // ---- content (scrolls); the sidebar is drawn over its left edge afterwards
        self.draw_title(c, x0, y);
        y += self.px(64);
        let mut hover_hits: Vec<(Rect, Hit)> = Vec::new();
        // tabs, when the page has several parts
        let tabs = self.page.tabs();
        let tab = self.tab.min(tabs.len().saturating_sub(1));
        if tabs.len() > 1 {
            let (mut tx, mut ty) = (x0, y);
            for (i, name) in tabs.iter().enumerate() {
                let tw = f.base.width(name) + self.px(32);
                if tx + tw > x0 + cw && tx > x0 {
                    tx = x0;
                    ty += self.px(44);
                }
                let r = (tx, ty, tw, self.px(36));
                if i == tab {
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(18) as f32, p.primary_container);
                } else if inside(&r, self.pointer) {
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(18) as f32, p.surface_container_highest);
                } else {
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(18) as f32, p.surface_container);
                }
                f.base.draw_centered(c, r.0 + r.2 / 2, r.1 + r.3 / 2, name, if i == tab { p.on_primary_container } else { p.on_surface });
                hover_hits.push((r, Hit::Tab(i)));
                tx += tw + self.px(8);
            }
            y = ty + self.px(54);
        }
        for sec in self.page.sections.iter().filter(|s| tabs.len() <= 1 || s.tab == tabs[tab]) {
            f.small.draw(c, x0 + self.px(6), y + self.px(14), &sec.title.to_uppercase(), alpha(p.on_surface_variant, 0xe0));
            y += self.px(32);
            let card_h: i32 = sec.rows.iter().map(|r| self.row_h(r)).sum();
            c.rounded_rect(x0, y, cw, card_h, self.px(14) as f32, p.surface_container);
            let mut ry = y;
            for (i, row) in sec.rows.iter().enumerate() {
                let rh = self.row_h(row);
                if i > 0 {
                    c.fill_rect(x0 + self.px(20), ry, cw - self.px(40), 1.max(self.px(1)), alpha(p.outline_variant, 0x60));
                }
                self.draw_row(c, row, x0, ry, cw, rh, &mut hover_hits);
                ry += rh;
            }
            y += card_h + self.px(26);
        }
        let content_height = y + self.scroll + self.px(20);
        hits.extend(hover_hits);

        // ---- sidebar
        c.fill_rect(0, 0, side_w, self.h, p.surface_container);
        if !compact {
            f.base.draw(c, self.px(22), self.px(34), "Settings", p.on_surface);
        }
        let mut ny = self.px(66);
        for (i, (_, title, icon)) in PAGES.iter().enumerate() {
            let r = (self.px(if compact { 8 } else { 10 }), ny, side_w - self.px(if compact { 16 } else { 20 }), self.px(42));
            if i == self.nav {
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(10) as f32, p.primary_container);
            } else if inside(&r, self.pointer) {
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(10) as f32, alpha(p.surface_container_highest, 0xa0));
            }
            let col = if i == self.nav { p.on_primary_container } else { p.on_surface };
            if compact {
                f.icon.draw_centered(c, r.0 + r.2 / 2, r.1 + r.3 / 2, icon, col);
            } else {
                f.icon.draw(c, r.0 + self.px(14), r.1 + r.3 / 2, icon, col);
                f.base.draw(c, r.0 + self.px(46), r.1 + r.3 / 2, &f.base.fit(title, r.2 - self.px(54)), col);
            }
            hits.push((r, Hit::Nav(i)));
            ny += self.px(46);
        }
        // a thin scrollbar
        let view = self.h;
        if content_height > view {
            let track = view - self.px(16);
            let bar_h = ((view as f32 / content_height as f32) * track as f32).max(self.px(30) as f32) as i32;
            let max_scroll = (content_height - view).max(1);
            let by = self.px(8) + ((self.scroll as f32 / max_scroll as f32) * (track - bar_h) as f32) as i32;
            c.rounded_rect(self.w - self.px(8), by, self.px(4), bar_h, self.px(2) as f32, alpha(p.on_surface_variant, 0x70));
        }

        // ---- status line
        if !self.status.is_empty() {
            let tw = f.small.width(self.status) + self.px(28);
            let sx = side_w + (self.w - side_w - tw) / 2;
            c.rounded_rect(sx, self.h - self.px(52), tw, self.px(34), self.px(17) as f32, p.surface_container_highest);
            f.small.draw_centered(c, sx + tw / 2, self.h - self.px(35), self.status, p.on_surface);
        }

        // ---- open dropdown on top of everything
        if let Some(d) = self.drop {
            self.draw_dropdown(c, d, &mut hits);
        }
        Drawn { hits, content_height }
    }

    fn draw_title(&self, c: &mut Canvas, x: i32, y: i32) {
        self.fonts.title.draw(c, x, y + self.px(18), self.page.title, self.pal.on_surface);
    }

    fn row_h(&self, r: &crate::model::Row) -> i32 {
        self.px(if r.desc.is_empty() { 54 } else { 66 })
    }

    fn draw_row(&self, c: &mut Canvas, row: &crate::model::Row, x: i32, y: i32, cw: i32, rh: i32, hits: &mut Vec<(Rect, Hit)>) {
        let (p, f) = (self.pal, self.fonts);
        let right = x + cw - self.px(20);
        let mid = y + rh / 2;
        let ctl_w = self.draw_ctl(c, &row.ctl, right, mid, hits);
        let text_w = cw - self.px(40) - ctl_w - self.px(16);
        let title = f.base.fit(&row.title, text_w.max(self.px(80)));
        if row.desc.is_empty() {
            f.base.draw(c, x + self.px(20), mid, &title, p.on_surface);
        } else {
            f.base.draw(c, x + self.px(20), mid - self.px(11), &title, p.on_surface);
            f.small.draw(c, x + self.px(20), mid + self.px(12), &f.small.fit(&row.desc, text_w.max(self.px(80))), p.on_surface_variant);
        }
    }

    /// Draw a control right-aligned to `right`, centred on `mid`; returns the width it took.
    fn draw_ctl(&self, c: &mut Canvas, ctl: &Ctl, right: i32, mid: i32, hits: &mut Vec<(Rect, Hit)>) -> i32 {
        let (p, f) = (self.pal, self.fonts);
        let bh = self.px(34);
        let hovered = |r: &Rect| inside(r, self.pointer);
        match ctl {
            Ctl::Toggle { path, on } => {
                let (tw, th) = (self.px(46), self.px(26));
                let r = (right - tw, mid - th / 2, tw, th);
                c.rounded_rect(r.0, r.1, r.2, r.3, th as f32 / 2.0, if *on { p.primary } else { p.surface_container_highest });
                if !*on {
                    c.rounded_outline(r.0, r.1, r.2, r.3, th as f32 / 2.0, 1.5, p.outline);
                }
                let knob = if *on { self.px(20) } else { self.px(14) };
                let kx = if *on { r.0 + r.2 - self.px(3) - knob } else { r.0 + self.px(6) };
                c.circle(kx + knob / 2, mid, knob / 2, if *on { p.on_primary } else { p.outline });
                hits.push((r, Hit::Toggle(path.clone(), !*on)));
                tw
            }
            Ctl::Choice { path, options, cur } => {
                let label = options.iter().find(|(v, _)| v.same(cur)).map(|(_, l)| l.clone()).unwrap_or_else(|| match cur {
                    Val::Str(s) if !s.is_empty() => s.clone(),
                    Val::Int(i) => i.to_string(),
                    Val::Float(f) => f.to_string(),
                    _ => "Default".into(),
                });
                let label = f.base.fit(&label, self.px(240));
                let w = (f.base.width(&label) + self.px(56)).max(self.px(150));
                let r = (right - w, mid - bh / 2, w, bh);
                let open = self.drop.is_some_and(|d| &d.path == path);
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, if hovered(&r) || open { p.surface_container_highest } else { p.surface_container_high });
                f.base.draw(c, r.0 + self.px(14), mid, &label, p.on_surface);
                f.small.draw(c, r.0 + r.2 - self.px(26), mid, "\u{f0140}", p.on_surface_variant);
                hits.push((r, Hit::Drop { path: path.clone(), options: options.clone(), cur: cur.clone(), anchor: r }));
                w
            }
            Ctl::Number { path, val, min, max, step, int, unit } => {
                let bw = self.px(34);
                let editing = self.edit.filter(|e| &e.path == path);
                let shown = match editing {
                    Some(e) => format!("{}▏", e.text),
                    None if *int => format!("{}{unit}", *val as i64),
                    None => format!("{}{unit}", (*val * 1000.0).round() / 1000.0),
                };
                let vw = self.px(110).max(f.base.width(&shown) + self.px(20));
                let total = bw * 2 + vw + self.px(8);
                let minus = (right - total, mid - bh / 2, bw, bh);
                let value = (right - total + bw + self.px(4), mid - bh / 2, vw, bh);
                let plus = (right - bw, mid - bh / 2, bw, bh);
                for (r, label) in [(&minus, "−"), (&plus, "+")] {
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, if hovered(r) { p.surface_container_highest } else { p.surface_container_high });
                    f.base.draw_centered(c, r.0 + r.2 / 2, mid, label, p.on_surface);
                }
                c.rounded_rect(value.0, value.1, value.2, value.3, self.px(9) as f32, p.surface_container_high);
                if editing.is_some() {
                    c.rounded_outline(value.0, value.1, value.2, value.3, self.px(9) as f32, 1.5, p.primary);
                }
                f.base.draw_centered(c, value.0 + value.2 / 2, mid, &shown, p.on_surface);
                let st = |d: f64| Hit::Step { path: path.clone(), cur: *val, delta: d, min: *min, max: *max, int: *int };
                hits.push((minus, st(-step)));
                hits.push((plus, st(*step)));
                hits.push((value, Hit::Edit { path: path.clone(), initial: if *int { format!("{}", *val as i64) } else { format!("{}", (*val * 1000.0).round() / 1000.0) }, numeric: Some((*min, *max, *int)) }));
                total
            }
            Ctl::Text { path, val, hint } => {
                let w = self.px(250);
                let r = (right - w, mid - bh / 2, w, bh);
                let editing = self.edit.filter(|e| &e.path == path);
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, p.surface_container_high);
                if editing.is_some() {
                    c.rounded_outline(r.0, r.1, r.2, r.3, self.px(9) as f32, 1.5, p.primary);
                }
                let inner = w - self.px(24);
                match editing {
                    Some(e) => {
                        // show the end of what is being typed
                        let mut t = e.text.clone();
                        while f.base.width(&t) > inner - self.px(6) && !t.is_empty() {
                            t.remove(0);
                        }
                        f.base.draw(c, r.0 + self.px(12), mid, &format!("{t}▏"), p.on_surface);
                    }
                    None if val.is_empty() => f.base.draw(c, r.0 + self.px(12), mid, &f.base.fit(hint, inner), alpha(p.on_surface_variant, 0xa0)),
                    None => f.base.draw(c, r.0 + self.px(12), mid, &f.base.fit(val, inner), p.on_surface),
                }
                hits.push((r, Hit::Edit { path: path.clone(), initial: val.clone(), numeric: None }));
                w
            }
            Ctl::Slider { path, val, min, max, int, unit } => {
                let w = self.px(300);
                let r = (right - w, mid - self.px(14), w, self.px(28));
                let label = format!("{}{unit}", *val as i64);
                let lw = f.base.width("200%").max(f.base.width(&label)) + self.px(10);
                let track_x = r.0;
                let track_w = w - lw - self.px(12);
                let frac = ((*val - *min) / (*max - *min).max(1e-9)).clamp(0.0, 1.0);
                let th = self.px(6);
                c.rounded_rect(track_x, mid - th / 2, track_w, th, th as f32 / 2.0, p.surface_container_highest);
                c.rounded_rect(track_x, mid - th / 2, (track_w as f64 * frac) as i32, th, th as f32 / 2.0, p.primary);
                let kx = track_x + (track_w as f64 * frac) as i32;
                c.circle(kx, mid, self.px(9), p.primary);
                f.base.draw(c, r.0 + w - lw + self.px(6), mid, &label, p.on_surface);
                // the grabbable area is the track plus a margin for the knob
                hits.push(((track_x - self.px(10), r.1, track_w + self.px(20), r.3), Hit::Slider { path: path.clone(), min: *min, max: *max, int: *int, pad: self.px(10) }));
                w
            }
            Ctl::Button { label, act } => {
                let label = f.base.fit(label, self.px(380));
                let w = f.base.width(&label) + self.px(36);
                let r = (right - w, mid - bh / 2, w, bh);
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, if hovered(&r) { p.primary } else { p.primary_container });
                f.base.draw_centered(c, r.0 + r.2 / 2, mid, &label, if hovered(&r) { p.on_primary } else { p.on_primary_container });
                hits.push((r, Hit::Act(act.clone())));
                w
            }
            Ctl::BarItem { id, shown, left, right: can_right } => {
                // the show/hide switch on the far right, then the two arrows
                let (tw, th) = (self.px(46), self.px(26));
                let sw = (right - tw, mid - th / 2, tw, th);
                c.rounded_rect(sw.0, sw.1, sw.2, sw.3, th as f32 / 2.0, if *shown { p.primary } else { p.surface_container_highest });
                if !*shown {
                    c.rounded_outline(sw.0, sw.1, sw.2, sw.3, th as f32 / 2.0, 1.5, p.outline);
                }
                let knob = if *shown { self.px(20) } else { self.px(14) };
                let kx = if *shown { sw.0 + sw.2 - self.px(3) - knob } else { sw.0 + self.px(6) };
                c.circle(kx + knob / 2, mid, knob / 2, if *shown { p.on_primary } else { p.outline });
                hits.push((sw, Hit::Act(Act::BarItem(id, 0))));
                let bw = self.px(34);
                let mut x = sw.0 - self.px(16) - bw;
                for (glyph, enabled, dir) in [("\u{f0142}", *can_right, 1i8), ("\u{f0141}", *left, -1i8)] {
                    let r = (x, mid - bh / 2, bw, bh);
                    let hot = enabled && hovered(&r);
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, if hot { p.primary } else { p.surface_container_high });
                    let col = if !enabled { alpha(p.on_surface_variant, 0x50) } else if hot { p.on_primary } else { p.on_surface };
                    f.icon.draw_centered(c, r.0 + r.2 / 2, mid, glyph, col);
                    if enabled {
                        hits.push((r, Hit::Act(Act::BarItem(id, dir))));
                    }
                    x -= bw + self.px(6);
                }
                sw.2 + self.px(16) + bw * 2 + self.px(6)
            }
            Ctl::Cursors { theme, size } => {
                // a few pointers of the theme, side by side
                let shapes = cursor_preview(theme, *size);
                let gap = self.px(18);
                let total: i32 = shapes.iter().map(|i| i.w).sum::<i32>() + gap * shapes.len().saturating_sub(1) as i32;
                let mut x = right - total;
                for img in &shapes {
                    c.image(x, mid - img.h / 2, img);
                    x += img.w + gap;
                }
                if shapes.is_empty() {
                    f.small.draw(c, right - f.small.width("the built-in arrow"), mid, "the built-in arrow", p.on_surface_variant);
                }
                total.max(self.px(120))
            }
            Ctl::Buttons(buttons) => {
                // right to left, so the first one ends up leftmost
                let mut x = right;
                let mut total = 0;
                for (label, act) in buttons.iter().rev() {
                    let label = f.base.fit(label, self.px(160));
                    let w = f.base.width(&label) + self.px(28);
                    let r = (x - w, mid - bh / 2, w, bh);
                    let hot = hovered(&r);
                    c.rounded_rect(r.0, r.1, r.2, r.3, self.px(9) as f32, if hot { p.primary } else { p.primary_container });
                    f.base.draw_centered(c, r.0 + r.2 / 2, mid, &label, if hot { p.on_primary } else { p.on_primary_container });
                    hits.push((r, Hit::Act(act.clone())));
                    x -= w + self.px(8);
                    total += w + self.px(8);
                }
                total
            }
            Ctl::Info(text) => {
                let t = f.base.fit(text, self.px(420));
                let w = f.base.width(&t);
                f.base.draw(c, right - w, mid, &t, p.on_surface_variant);
                w
            }
        }
    }

    fn draw_dropdown(&self, c: &mut Canvas, d: &DropState, hits: &mut Vec<(Rect, Hit)>) {
        let (p, f) = (self.pal, self.fonts);
        // clicking anywhere else closes it
        hits.push(((0, 0, self.w, self.h), Hit::CloseDrop));
        let item_h = self.px(36);
        let widest = d.options.iter().map(|(_, l)| f.base.width(l)).max().unwrap_or(0);
        let w = (widest + self.px(48)).max(d.anchor.2);
        let total = d.options.len() as i32 * item_h + self.px(12);
        let max_h = self.h - self.px(24);
        let h = total.min(max_h);
        let mut x = d.anchor.0 + d.anchor.2 - w;
        x = x.clamp(self.px(8), (self.w - w - self.px(8)).max(self.px(8)));
        let below = d.anchor.1 + d.anchor.3 + self.px(6);
        let y = if below + h <= self.h - self.px(8) { below } else { (d.anchor.1 - h - self.px(6)).max(self.px(8)) };
        c.rounded_rect(x - 1, y - 1, w + 2, h + 2, self.px(12) as f32, alpha(p.outline_variant, 0xff));
        c.rounded_rect(x, y, w, h, self.px(11) as f32, p.surface_container_highest);
        // keep the selected option in view in long lists
        let visible = ((h - self.px(12)) / item_h).max(1) as usize;
        let sel = d.options.iter().position(|(v, _)| v.same(&d.cur)).unwrap_or(0);
        let first = sel.saturating_sub(visible / 2).min(d.options.len().saturating_sub(visible));
        for (slot, (val, label)) in d.options.iter().skip(first).take(visible).enumerate() {
            let r = (x + self.px(6), y + self.px(6) + slot as i32 * item_h, w - self.px(12), item_h);
            let selected = val.same(&d.cur);
            if selected {
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(8) as f32, p.primary_container);
            } else if inside(&r, self.pointer) {
                c.rounded_rect(r.0, r.1, r.2, r.3, self.px(8) as f32, alpha(p.on_surface, 0x18));
            }
            f.base.draw(c, r.0 + self.px(12), r.1 + r.3 / 2, label, if selected { p.on_primary_container } else { p.on_surface });
            hits.push((r, Hit::Pick(d.path.clone(), val.clone())));
        }
        // swallow clicks inside the popup's padding
        hits.push(((x, y, w, h), Hit::CloseDrop));
        // ...but the option rows above must win: re-add them after the swallowing rect
        let rows: Vec<_> = hits.iter().filter(|(_, hh)| matches!(hh, Hit::Pick(..))).cloned().collect();
        hits.extend(rows);
    }
}


thread_local! {
    static CURSOR_PREVIEWS: std::cell::RefCell<std::collections::HashMap<(String, u32), Vec<Image>>> = Default::default();
}

/// The arrow, text and hand pointers of `theme` at about `size` pixels (cached: the files are only read once).
fn cursor_preview(theme: &str, size: u32) -> Vec<Image> {
    if theme.is_empty() {
        return Vec::new();
    }
    CURSOR_PREVIEWS.with(|c| {
        c.borrow_mut()
            .entry((theme.to_owned(), size))
            .or_insert_with(|| {
                std::env::set_var("XCURSOR_PATH", std::env::join_paths(vesper_config::cursor_dirs()).unwrap_or_default());
                let xt = xcursor::CursorTheme::load(theme);
                ["default", "text", "pointer", "wait"]
                    .iter()
                    .filter_map(|name| {
                        let images = std::fs::read(xt.load_icon(name)?).ok().and_then(|d| xcursor::parser::parse_xcursor(&d))?;
                        let best = images.iter().min_by_key(|i| (i.size as i64 - (size * 2) as i64).abs())?.size;
                        let img = images.iter().find(|i| i.size == best)?;
                        Some(Image::from_rgba(img.width as i32, img.height as i32, &img.pixels_rgba, true))
                    })
                    .collect()
            })
            .clone()
    })
}

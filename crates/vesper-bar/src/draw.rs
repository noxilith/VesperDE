//! Bar layout and drawing, on top of vesper-gfx.
//!
//! Left: workspace pills. Centre: icons of minimized apps (click to restore). Right: clock.

use vesper_apps::{Apps, Icons};
use vesper_config::theme::{alpha, with_alpha, Palette};
use vesper_gfx::{Canvas, Image, Rgba, Text};
use vesper_ipc::State;

use crate::icons;
use crate::shape::{self, Shape};
use crate::sys::{Battery, Net, Sys, Volume};

const TILE_TEXT: Rgba = [0xff, 0xff, 0xff, 0xf0];
const ICON_GAP: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    Workspace(usize),
    /// Index into `State::minimized`.
    Restore(usize),
    /// The volume control: click to mute, scroll to change.
    Volume,
    /// Screen brightness: scroll to change.
    Brightness,
    /// A plugin widget (its id); the click runs the plugin's `on_click`.
    Plugin(&'static str),
    /// The live-microphone indicator: click to mute.
    Mic,
    /// The clock: opens the calendar.
    Clock,
    /// Battery details.
    Battery,
    /// Wi-Fi networks.
    Network,
    /// Removable drives.
    Drives,
    /// Keyboard layout indicator: click to switch.
    Keyboard,
    MediaPrev,
    MediaPlay,
    MediaNext,
    /// A tray icon, by index into `Sys::tray`.
    Tray(usize),
    /// Notification centre.
    Bell,
    /// The system (distribution) icon.
    System,
    /// The gap between two icons: click to split the bar there (or join it again). The id names the icon to its left.
    Gap(&'static str),
}

pub struct Renderer {
    pub(crate) pal: Palette,
    pub(crate) text: Text,
    tile_text: Text,
    apps: Apps,
    icons: Icons,
    icon_size: i32,
    /// Background alpha, 0..255.
    pub(crate) bg_alpha: u8,
    pub style: Style,
    /// Colour for inactive workspaces when they follow the border colour.
    ws_inactive: Option<Rgba>,
    /// How far the volume slider goes: 100, or 150 with the volume boost on.
    pub(crate) volume_max: u32,
    /// Ids of the plugin widgets that exist (they are valid items in the bar lists).
    pub plugin_ids: Vec<&'static str>,
    /// Width the middle items may use (set while laying out).
    avail: i32,
    /// Icons after which the bar is split (see `Hit::Gap`).
    pub splits: Vec<String>,
    /// Rendered status icons (battery, wifi) by their settings.
    svg_cache: std::collections::HashMap<String, Image>,
    /// The system icon as last built: (settings it was made for, image or a fallback glyph).
    sys_cache: Option<((String, i32, Option<Rgba>), Result<Image, char>)>,
}

impl Renderer {
    /// Icon size for the minimized apps: `custom` pixels, or (0) a bit under the bar height.
    fn icon_px(bar_height: u32, custom: u32) -> i32 {
        if custom == 0 { ((bar_height as f32 - 8.0) * 0.9).round() as i32 } else { custom as i32 }.max(12)
    }

    /// Apply a changed icon size setting. Returns whether it changed.
    pub fn set_icon_size(&mut self, bar_height: u32, custom: u32) -> bool {
        let px = Self::icon_px(bar_height, custom);
        let changed = px != self.icon_size;
        self.icon_size = px;
        self.tile_text = self.text.resized(px as f32 * 0.6);
        changed
    }

    pub fn new(family: &str, size: f32, bar_height: u32, icon_size: u32) -> Self {
        let icon_size = Self::icon_px(bar_height, icon_size);
        let text = Text::new(family, size);
        Self {
            pal: Palette::load(),
            text: text.clone(),
            tile_text: text.resized(icon_size as f32 * 0.6),
            apps: Apps::load(),
            icons: Icons::default(),
            icon_size,
            bg_alpha: 255,
            style: Style::from_config(&vesper_config::Bar::default()),
            ws_inactive: None,
            avail: 0,
            volume_max: 100,
            plugin_ids: Vec::new(),
            splits: Vec::new(),
            svg_cache: Default::default(),
            sys_cache: None,
        }
    }

    /// Set the bar background opacity from a percentage. Returns whether it changed.
    pub fn set_opacity(&mut self, percent: u32) -> bool {
        let a = (percent.clamp(0, 100) * 255 / 100) as u8;
        let changed = a != self.bg_alpha;
        self.bg_alpha = a;
        changed
    }

    /// Draw the system icon with its left edge at `x`, `px` pixels wide.
    /// Make sure the system icon for these settings is built; returns its width in pixels.
    fn ensure_system_icon(&mut self, px: i32) -> i32 {
        let st = self.style.clone();
        let color = self.system_icon_tint();
        let key = (st.system_icon.clone(), px, color);
        if self.sys_cache.as_ref().map(|(k, _)| k) != Some(&key) {
            let built = self.build_system_icon(&st.system_icon, px, color);
            self.sys_cache = Some((key, built));
        }
        match &self.sys_cache.as_ref().unwrap().1 {
            Ok(img) => img.w,
            Err(_) => px,
        }
    }

    /// The colour the system icon is painted in: the accent, your own colour, or None (its own colours).
    fn system_icon_tint(&self) -> Option<Rgba> {
        if self.style.system_icon_accent { Some(self.pal.primary) } else { self.style.system_icon_color }
    }

    fn draw_system_icon(&mut self, c: &mut Canvas, x: i32, mid: i32, px: i32) {
        let tint = self.system_icon_tint().unwrap_or(self.pal.on_surface);
        match &self.sys_cache.as_ref().unwrap().1 {
            Ok(img) => c.image(x, mid - img.h / 2, img),
            Err(glyph) => {
                let big = self.text.resized(px as f32);
                big.draw_centered(c, x + px / 2, mid, &glyph.to_string(), tint);
            }
        }
    }

    /// The picture for a `system_icon` setting, or the glyph to draw when there is no usable file.
    fn build_system_icon(&mut self, source: &str, px: i32, color: Option<Rgba>) -> Result<Image, char> {
        let os = vesper_apps::distro::os_release();
        let fallback = vesper_apps::distro::glyph_for(&os);
        if let Some(id) = source.strip_prefix("glyph:") {
            let g = vesper_apps::distro::GLYPHS.iter().find(|(gid, _, _)| *gid == id).map_or(fallback, |(_, _, g)| *g);
            return Err(g);
        }
        // bundled logos: "bundled:singularitysys" picks the dark or light variant to suit the bar colour
        let dark_bar = (self.pal.surface[0] as u32 * 299 + self.pal.surface[1] as u32 * 587 + self.pal.surface[2] as u32 * 114) / 1000 < 128;
        let bundled = |name: &str| -> String {
            let full = if name == "singularitysys" { if dark_bar { "singularitysys-dark" } else { "singularitysys-light" } } else { name };
            vesper_apps::distro::bundled_path(full).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
        };
        // which file or icon name to load
        let key: String = if source == "auto" || source.is_empty() {
            let logo = os.get("LOGO").cloned().unwrap_or_default();
            let id = os.get("ID").cloned().unwrap_or_default();
            if !logo.is_empty() && self.icons.get(&logo, px).is_some() {
                logo
            } else if !id.is_empty() && self.icons.get(&id, px).is_some() {
                id
            } else if vesper_apps::distro::is_singularity(&os) {
                bundled("singularitysys")
            } else {
                return Err(fallback);
            }
        } else if let Some(name) = source.strip_prefix("bundled:") {
            bundled(name)
        } else {
            source.to_owned()
        };
        let Some(mut img) = self.icons.get(&key, px) else { return Err(fallback) };
        // a logo that is wider than tall fills the height: render it larger and cut away the empty margins
        let (_, bh) = alpha_extent(&img);
        if bh > 0 && bh < px - 1 {
            let bigger = ((px as f32 * px as f32 / bh as f32).round() as i32).clamp(px, px * 4);
            if let Some(i) = self.icons.get(&key, bigger) {
                img = i;
            }
        }
        let mut img = trim(&img);
        if let Some(col) = color {
            // recolour: keep the shape (alpha), replace the colour
            for p in img.data.chunks_exact_mut(4) {
                let a = p[3] as u32;
                p[0] = (col[2] as u32 * a / 255) as u8;
                p[1] = (col[1] as u32 * a / 255) as u8;
                p[2] = (col[0] as u32 * a / 255) as u8;
            }
        }
        Ok(img)
    }

    /// Draw one split mark (line, dots or dashes) centred on `cx`.
    fn split_mark(&self, c: &mut Canvas, cx: i32, mid: i32, color: Rgba, pct: i32) {
        let st = &self.style;
        let h = (st.bar_h * st.split_height / 100 * pct / 100).max(6);
        let w = st.split_width;
        let x = cx - w / 2;
        match st.split_style {
            SplitStyle::Line => c.fill_rect(x, mid - h / 2, w, h, color),
            SplitStyle::Dots => {
                let d = w + 1;
                for k in -1..=1 {
                    c.circle(cx, mid + k * (h / 3).max(d + 2), d.max(2) / 2 + 1, color);
                }
            }
            SplitStyle::Dashes => {
                let seg = (h / 5).max(2);
                for k in 0..3 {
                    c.fill_rect(x, mid - h / 2 + k * (h - seg) / 2, w, seg, color);
                }
            }
        }
    }

    /// Extra room left where the islands bar is split, so the two islands do not touch.
    /// Which part of the bar an item sits in: 0 left, 1 middle, 2 right.
    fn section(&self, id: &str) -> u8 {
        if self.style.right_items.iter().any(|i| i == id) {
            2
        } else if self.style.left_items.iter().any(|i| i == id) {
            0
        } else if self.style.middle_items.iter().any(|i| i == id) {
            1
        } else {
            2
        }
    }

    fn split_extra(&self, id: &str) -> i32 {
        if self.splits.iter().any(|s| s == id) { self.style.split_spacing } else { 0 }
    }

    /// Use new colours; returns true if they differ from the current ones.
    pub fn set_palette(&mut self, pal: Palette) -> bool {
        let changed = pal != self.pal;
        self.pal = pal;
        changed
    }

    /// Draw the whole bar. Returns the clickable regions as `(x0, x1, what)` and the shapes that were drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        canvas: &mut [u8],
        w: u32,
        h: u32,
        state: &State,
        clock: &str,
        sys: &Sys,
        hover: Option<Hit>,
    ) -> (Vec<(i32, i32, Hit)>, Vec<Shape>) {
        let wi = w as i32;
        let st = self.style.clone();
        let bar_h = st.bar_h;
        let top = if matches!(st.kind, Kind::Detached | Kind::Islands) { st.margin_top } else { 0 };
        let mid = top + bar_h / 2;
        let fill = alpha(self.pal.surface, self.bg_alpha);
        let mut border = st.border.unwrap_or(self.pal.primary);
        if st.border_blend {
            // the border is as see-through as the bar
            border[3] = (border[3] as u32 * self.bg_alpha as u32 / 255) as u8;
        }

        // the notch is at least as wide as its content needs
        let notch_w = if st.kind == Kind::Notch {
            let mut scratch = vec![0u8; canvas.len()];
            let (_, e) = self.content(&mut Canvas::new(&mut scratch, w, h), state, clock, sys, hover, mid, 0, 4000);
            let needed = (e.left_end - 8) + (4000 - e.right_start) + 32 + 40 + e.mid.map_or(0, |(a, b)| b - a + 40);
            st.notch_width.max(needed).min(wi)
        } else {
            st.notch_width
        };

        self.ws_inactive = st.ws_inactive;
        // where the content may sit horizontally
        let (lx, rx) = match st.kind {
            Kind::Fill => (14, wi - 14),
            Kind::Detached | Kind::Islands => (st.margin_side + 14, wi - st.margin_side - 14),
            Kind::Notch => {
                let nw = notch_w;
                let x0 = (wi - nw).max(0) / 2;
                (x0 + 16, x0 + nw.min(wi) - 16)
            }
        };

        // islands are as wide as their content, so measure it first on a scratch canvas
        let mut shapes: Vec<Shape> = Vec::new();
        if st.kind == Kind::Islands {
            let mut scratch = vec![0u8; canvas.len()];
            let (_, ext) = self.content(&mut Canvas::new(&mut scratch, w, h), state, clock, sys, hover, mid, lx, rx);
            // one island per run of icons: a new island starts where the icon group changes or you split the bar
            let r = st.radius;
            let mut runs: Vec<(i32, i32)> = Vec::new();
            let mut start: Option<i32> = None;
            for (i, it) in ext.items.iter().enumerate() {
                start.get_or_insert(it.1);
                let ends = match ext.items.get(i + 1) {
                    None => true,
                    Some(n) => self.section(n.0) != self.section(it.0) || self.splits.iter().any(|s| s == it.0),
                };
                if ends {
                    runs.push((start.take().unwrap(), it.2));
                }
            }
            let last = runs.len().saturating_sub(1);
            for (k, (a, b)) in runs.iter().enumerate() {
                let x0 = if k == 0 { st.margin_side } else { a - 12 };
                let x1 = if k == last { wi - st.margin_side } else { b + 12 };
                shapes.push(Shape::Round { x: x0, y: top, w: (x1 - x0).max(0), h: bar_h, r });
            }
        }

        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        match st.kind {
            Kind::Fill => {
                let shape = Shape::Rect { x: 0, y: 0, w: wi, h: bar_h };
                shape::paint(&mut c, &shape, fill, 0, border);
                // the border is a line along the bottom edge
                for i in 0..st.border_width.max(0) {
                    c.span(bar_h - 1 - i, 0, wi, border);
                }
                shapes.push(shape);
            }
            Kind::Detached => {
                let shape = Shape::Round { x: st.margin_side, y: top, w: (wi - 2 * st.margin_side).max(0), h: bar_h, r: st.radius };
                shape::paint(&mut c, &shape, fill, st.border_width, border);
                shapes.push(shape);
            }
            Kind::Notch => {
                let shape = Shape::Notch { x: (wi - notch_w).max(0) / 2, w: notch_w.min(wi), h: bar_h, r: st.radius };
                shape::paint(&mut c, &shape, fill, st.border_width, border);
                shapes.push(shape);
            }
            Kind::Islands => {
                for s in &shapes {
                    shape::paint(&mut c, s, fill, st.border_width, border);
                }
            }
        }
        let (hits, _) = self.content(&mut c, state, clock, sys, hover, mid, lx, rx);
        (hits, shapes)
    }

    /// The bar's contents between `lx` and `rx`, vertically centred on `mid`.
    #[allow(clippy::too_many_arguments)]
    fn content(
        &mut self,
        c: &mut Canvas,
        state: &State,
        clock: &str,
        sys: &Sys,
        hover: Option<Hit>,
        mid: i32,
        lx: i32,
        rx: i32,
    ) -> (Vec<(i32, i32, Hit)>, Extents) {
        let mut hits = Vec::new();

        self.avail = rx - lx;
        let mut items: Vec<(&'static str, i32, i32)> = Vec::new();
        let (lefts, mids, rights) = (self.style.left_items.clone(), self.style.middle_items.clone(), self.style.right_items.clone());
        let mut used: Vec<&'static str> = Vec::new();
        let plugin_ids = self.plugin_ids.clone();
        let mut known = |id: &String| -> Option<&'static str> {
            let id = ITEM_IDS.iter().chain(plugin_ids.iter()).copied().find(|k| k == id)?;
            if used.contains(&id) {
                return None; // listed twice: the first one wins
            }
            used.push(id);
            Some(id)
        };
        let rights: Vec<&'static str> = rights.iter().filter_map(&mut known).collect();
        let lefts: Vec<&'static str> = lefts.iter().filter_map(&mut known).collect();
        let mids: Vec<&'static str> = mids.iter().filter_map(&mut known).collect();

        // Right side, drawn right to left in the configured order
        let mut edge = rx;
        let mut first = true;
        for id in rights {
            let right = if first { edge } else { edge - 18 - self.split_extra(id) };
            if let Some((x0, x1)) = self.draw_item(id, c, right, mid, clock, sys, state, hover, &mut hits) {
                items.push((id, x0, x1));
                edge = x0;
                first = false;
            }
        }
        let right_start = edge; // everything to the right of here is taken

        // Left side, left to right: measure each item on a 1x1 canvas, then draw it
        let mut x = lx;
        let mut left_end = lx + 8;
        let mut first = true;
        for id in lefts {
            let Some(w) = self.item_width(id, mid, clock, sys, state) else { continue };
            if first && id == "system" {
                x -= 6; // the logo hugs the edge a little closer than the other items
            }
            first = false;
            if self.draw_item(id, c, x + w, mid, clock, sys, state, hover, &mut hits).is_some() {
                items.push((id, x, x + w));
                left_end = x + w;
                x += w + 18 + self.split_extra(id);
            }
        }

        // Middle: the items as one group, centred between the two sides
        let mut mid_range = None;
        let widths: Vec<(&'static str, i32)> = mids.iter().filter_map(|id| self.item_width(id, mid, clock, sys, state).map(|w| (*id, w))).collect();
        if !widths.is_empty() {
            let gaps: i32 = widths.iter().take(widths.len() - 1).map(|(id, _)| 18 + self.split_extra(id)).sum();
            let total: i32 = widths.iter().map(|(_, w)| w).sum::<i32>() + gaps;
            let x0 = (((lx + rx) - total) / 2).clamp(left_end + 16, (right_start - 16 - total).max(left_end + 16));
            let mut x = x0;
            for (id, w) in &widths {
                if self.draw_item(id, c, x + w, mid, clock, sys, state, hover, &mut hits).is_some() {
                    items.push((id, x, x + w));
                }
                x += w + 18 + self.split_extra(id);
            }
            mid_range = Some((x0, x0 + total));
        }
        // the focused window's title, centred in whatever room is left (not drawn over middle items)
        if self.style.show_title && mid_range.is_none() {
            if let Some(win) = state.focused.as_ref().filter(|w| !w.title.is_empty()) {
                let (a, b) = (left_end + 24, right_start - 24);
                let max = b - a;
                if max > 60 {
                    let mut t: String = win.title.clone();
                    while self.text.width(&t) > max && t.chars().count() > 1 {
                        t.pop();
                        t = t.trim_end().to_string();
                        if !t.ends_with('…') && self.text.width(&format!("{t}…")) <= max {
                            t.push('…');
                            break;
                        }
                    }
                    let w = self.text.width(&t);
                    let x = (((lx + rx) - w) / 2).clamp(a, (b - w).max(a));
                    self.text.draw(c, x, mid, &t, self.pal.primary);
                    mid_range = Some((x, x + w));
                }
            }
        }
        items.sort_by_key(|i| i.1);

        // the gaps between icons: hover one to see where the bar can be split, click to split it (or join it again)
        let mut gap_spans = Vec::new();
        for pair in items.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            // only between icons of the same section: never between the left, middle and right parts
            if b.1 - a.2 >= 6 && self.section(a.0) == self.section(b.0) {
                gap_spans.push((a.0, a.2, b.1));
            }
        }
        for (id, x0, x1) in &gap_spans {
            let split = self.splits.iter().any(|s| s == id);
            let cx = (x0 + x1) / 2;
            let hot = hover == Some(Hit::Gap(id));
            let mark_color = self.style.split_color.unwrap_or(with_alpha(self.pal.outline, 0.9));
            // hovering shows a smaller mark in the accent colour, whether the bar is split here or not
            if hot {
                self.split_mark(c, cx, mid, self.pal.primary, 60);
            } else if split && self.style.kind != Kind::Islands {
                self.split_mark(c, cx, mid, mark_color, 100);
            }
            // a narrow strip in the middle of the gap, so a click aimed at an icon never splits the bar
            hits.push(((cx - 3).max(*x0), (cx + 3).min(*x1), Hit::Gap(id)));
        }
        (hits, Extents { left_end, right_start, mid: mid_range, items })
    }
}


/// Where the three groups of content ended up (used to size the islands).
struct Extents {
    left_end: i32,
    right_start: i32,
    mid: Option<(i32, i32)>,
    /// Every icon group, left to right: (id, x0, x1).
    items: Vec<(&'static str, i32, i32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SplitStyle {
    Line,
    Dots,
    Dashes,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WsStyle {
    Dots,
    Halo,
    Numbers,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Fill,
    Detached,
    Notch,
    Islands,
}

/// How the bar looks, from the `[bar]` config section.
#[derive(Clone, Debug)]
pub struct Style {
    pub kind: Kind,
    pub bar_h: i32,
    pub border_width: i32,
    pub border: Option<Rgba>,
    pub border_blend: bool,
    pub ws_style: WsStyle,
    pub ws_color: Option<Rgba>,
    pub ws_occupied_only: bool,
    pub ws_inactive: Option<Rgba>,
    pub split_style: SplitStyle,
    pub split_width: i32,
    pub split_height: i32,
    pub split_color: Option<Rgba>,
    pub split_spacing: i32,
    pub status_ios: bool,
    pub system_icon: String,
    pub system_icon_size: i32,
    pub system_icon_accent: bool,
    pub system_icon_color: Option<Rgba>,
    pub radius: i32,
    pub margin_top: i32,
    pub margin_side: i32,
    pub notch_width: i32,
    pub right_items: Vec<String>,
    pub left_items: Vec<String>,
    pub middle_items: Vec<String>,
    pub clock_24h: bool,
    pub clock_seconds: bool,
    pub clock_date: bool,
    pub show_title: bool,
    /// Nominal size of every icon in the bar.
    pub item_icon: i32,
}

impl Style {
    pub fn from_config(b: &vesper_config::Bar) -> Self {
        let kind = match b.style.as_str() {
            "detached" => Kind::Detached,
            "notch" => Kind::Notch,
            "islands" => Kind::Islands,
            _ => Kind::Fill,
        };
        Style {
            kind,
            bar_h: b.height.max(16) as i32,
            border_width: b.border_width.clamp(0, 12),
            border: vesper_config::theme::hex(&b.border_color),
            border_blend: b.border_blend,
            ws_style: match b.workspace_style.as_str() {
                "halo" => WsStyle::Halo,
                "numbers" => WsStyle::Numbers,
                _ => WsStyle::Dots,
            },
            ws_color: vesper_config::theme::hex(&b.workspace_color),
            ws_occupied_only: b.workspace_mode == "occupied",
            ws_inactive: vesper_config::theme::hex(&b.workspace_inactive_color),
            split_style: match b.split_style.as_str() {
                "dots" => SplitStyle::Dots,
                "dashes" => SplitStyle::Dashes,
                _ => SplitStyle::Line,
            },
            split_width: b.split_width.clamp(1, 8),
            split_height: b.split_height.clamp(20, 100),
            split_color: vesper_config::theme::hex(&b.split_color),
            split_spacing: b.split_spacing.clamp(0, 40),
            status_ios: b.status_icons != "glyphs",
            system_icon: b.system_icon.clone(),
            system_icon_size: b.system_icon_size.clamp(0, 64) as i32,
            system_icon_accent: b.system_icon_accent,
            system_icon_color: vesper_config::theme::hex(&b.system_icon_color),
            radius: b.radius.clamp(0, 40),
            margin_top: b.margin_top.clamp(0, 80),
            margin_side: b.margin_side.clamp(0, 200),
            notch_width: b.notch_width.clamp(240, 2400),
            right_items: b.right_items.clone(),
            left_items: b.left_items.clone(),
            middle_items: b.middle_items.clone(),
            clock_24h: b.clock_24h,
            clock_seconds: b.clock_seconds,
            clock_date: b.clock_date,
            show_title: b.show_title,
            item_icon: b.item_icon_size.clamp(12, 40) as i32,
        }
    }

    /// The clock text for `now` in the configured format.
    pub fn clock_text(&self, now: chrono::DateTime<chrono::Local>) -> String {
        let time = match (self.clock_24h, self.clock_seconds) {
            (true, false) => "%H:%M",
            (true, true) => "%H:%M:%S",
            (false, false) => "%-I:%M %p",
            (false, true) => "%-I:%M:%S %p",
        };
        // the time, then (after a \u{1} the bar draws smaller and dimmer) the weekday and day
        let fmt = if self.clock_date { format!("{time}\u{1}%a %d") } else { time.to_string() };
        now.format(&fmt).to_string()
    }

    /// Height of the whole surface: the bar plus the gap above it for the floating styles.
    pub fn surface_height(&self) -> i32 {
        match self.kind {
            Kind::Detached | Kind::Islands => self.bar_h + self.margin_top,
            _ => self.bar_h,
        }
    }
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

const GREEN: Rgba = [0x6a, 0xd9, 0x8f, 0xff];

/// Every item that can sit on the right of the bar.
pub const ITEM_IDS: [&str; 14] = ["drives", "mic", "system", "workspaces", "minimized", "media", "keyboard", "tray", "brightness", "volume", "network", "battery", "clock", "bell"];

impl Renderer {
    /// Draw one bar item so that it ends at `right`, with its clickable parts. Returns its span, or None if it has nothing to show.
    #[allow(clippy::too_many_arguments)]
    fn draw_item(
        &mut self,
        id: &str,
        c: &mut Canvas,
        right: i32,
        mid: i32,
        clock: &str,
        sys: &Sys,
        state: &State,
        hover: Option<Hit>,
        hits: &mut Vec<(i32, i32, Hit)>,
    ) -> Option<(i32, i32)> {
        let span = self.draw_item_raw(id, c, right, mid, clock, sys, state, hover, hits)?;
        let (x0, x1) = span;
        match id {
            "clock" => hits.push((x0 - 6, x1 + 6, Hit::Clock)),
            "battery" => hits.push((x0 - 6, x1 + 6, Hit::Battery)),
            "network" => hits.push((x0 - 6, x1 + 6, Hit::Network)),
            _ => {}
        }
        Some(span)
    }

    /// How wide an item is right now (None = nothing to show): drawn once onto a 1x1 canvas, far off to the right.
    fn item_width(&mut self, id: &str, mid: i32, clock: &str, sys: &Sys, state: &State) -> Option<i32> {
        let mut px = [0u8; 4];
        let mut tiny = Canvas::new(&mut px, 1, 1);
        let (x0, x1) = self.draw_item_raw(id, &mut tiny, 20_000, mid, clock, sys, state, None, &mut Vec::new())?;
        Some(x1 - x0)
    }

    /// The drawing of one item, ending at `right`.
    #[allow(clippy::too_many_arguments)]
    fn draw_item_raw(
        &mut self,
        id: &str,
        c: &mut Canvas,
        right: i32,
        mid: i32,
        clock: &str,
        sys: &Sys,
        state: &State,
        hover: Option<Hit>,
        hits: &mut Vec<(i32, i32, Hit)>,
    ) -> Option<(i32, i32)> {
        let accent = self.pal.primary;
        let hot = |h: Hit| hover == Some(h);
        let gt = self.text.resized(self.style.item_icon as f32 * 0.95); // glyph icons follow the icon size
        match id {
            pid if pid.starts_with("plugin:") => {
                // a plugin widget: "text", or "glyph|text"
                let raw = sys.plugin.get(pid).map(String::as_str).unwrap_or("");
                if raw.is_empty() {
                    return None;
                }
                let (glyph, label) = match raw.split_once('|') {
                    Some((g, t)) if g.chars().count() <= 2 => (Some(g), t.trim()),
                    _ => (None, raw),
                };
                let colour = if hot(Hit::Plugin(vesper_config::intern(pid))) { with_alpha(accent, 0.7) } else { accent };
                let mut x = right;
                if !label.is_empty() {
                    x -= self.text.width(label);
                    self.text.draw(c, x, mid, label, colour);
                }
                if let Some(g) = glyph {
                    x -= gt.width(g) + if label.is_empty() { 0 } else { 6 };
                    gt.draw(c, x, mid, g, colour);
                }
                hits.push((x - 6, right + 6, Hit::Plugin(vesper_config::intern(pid))));
                Some((x, right))
            }
            "bell" => {
                let (glyph, hit, lit) = (if sys.notif.dnd { "\u{f009b}" } else { "\u{f009a}" }, Hit::Bell, with_alpha(accent, 0.75));
                let w = gt.width(glyph);
                let x = right - w;
                let muted = with_alpha(accent, 0.45); // same as the muted speaker
                let colour = if id == "bell" && sys.notif.dnd { muted } else { accent };
                gt.draw(c, x, mid, glyph, if hot(hit) { if id == "bell" && sys.notif.dnd { with_alpha(accent, 0.6) } else { lit } } else { colour });
                hits.push((x - 6, right + 6, hit));
                if id == "bell" && sys.notif.unread > 0 && !sys.notif.dnd {
                    // a small badge with the unread count
                    let label = if sys.notif.unread > 9 { "9+".to_string() } else { sys.notif.unread.to_string() };
                    let d = 13;
                    c.rounded_rect(right - 6, mid - d - 1, d, d, d as f32 / 2.0, self.pal.error);
                    let small = self.text.resized(9.0);
                    small.draw_centered(c, right - 6 + d / 2, mid - d / 2 - 1, &label, self.pal.on_primary);
                }
                Some((x, right))
            }
            "drives" => {
                // only while a removable drive is plugged in
                if sys.drives.is_empty() {
                    return None;
                }
                let glyph = "\u{f02ca}";
                let x = right - gt.width(glyph);
                gt.draw(c, x, mid, glyph, if hot(Hit::Drives) { with_alpha(accent, 0.7) } else { accent });
                hits.push((x - 6, right + 6, Hit::Drives));
                Some((x, right))
            }
            "mic" => {
                // only while the microphone is live
                if sys.mic != Some(true) {
                    return None;
                }
                let glyph = "\u{f036c}";
                let x = right - gt.width(glyph);
                gt.draw(c, x, mid, glyph, if hot(Hit::Mic) { with_alpha(accent, 0.7) } else { accent });
                hits.push((x - 6, right + 6, Hit::Mic));
                Some((x, right))
            }
            "media" => {
                // no player running: just a music note
                let Some(m) = sys.media.clone() else {
                    let glyph = "\u{f0387}";
                    let x = right - gt.width(glyph);
                    gt.draw(c, x, mid, glyph, with_alpha(accent, 0.6));
                    return Some((x, right));
                };
                let label = if m.artist.is_empty() { m.title.clone() } else { format!("{} \u{2013} {}", m.artist, m.title) };
                let label = self.text.fit(&label, 170);
                let (prev, next, play) = ("\u{f04ae}", "\u{f04ad}", if m.playing { "\u{f03e4}" } else { "\u{f040a}" });
                let (pw, ow, nw) = (gt.width(prev), gt.width(play), gt.width(next));
                let lw = if label.is_empty() { 0 } else { self.text.width(&label) + 12 };
                let total = pw + 10 + ow + 10 + nw + lw;
                let mut x = right - total;
                let x0 = x;
                for (glyph, w, hit) in [(prev, pw, Hit::MediaPrev), (play, ow, Hit::MediaPlay), (next, nw, Hit::MediaNext)] {
                    gt.draw(c, x, mid, glyph, if hot(hit) { with_alpha(accent, 0.7) } else { accent });
                    hits.push((x - 5, x + w + 5, hit));
                    x += w + 10;
                }
                if lw > 0 {
                    self.text.draw(c, x + 2, mid, &label, accent);
                }
                Some((x0, right))
            }
            "tray" => {
                if sys.tray.is_empty() {
                    return None;
                }
                let s = self.style.item_icon;
                let n = sys.tray.len() as i32;
                let total = n * s + (n - 1) * 8;
                let x0 = right - total;
                for (i, t) in sys.tray.iter().enumerate() {
                    let x = x0 + i as i32 * (s + 8);
                    // tray icons keep their own colours
                    let img = match &t.pixmap {
                        Some((w, h, rgba)) => Some(Image::from_rgba(*w, *h, rgba, false)),
                        None => self.icons.get(&t.icon_name, s).or_else(|| {
                            let f = format!("{}/{}.svg", t.theme_path, t.icon_name);
                            std::fs::read_to_string(f).ok().and_then(|svg| vesper_apps::icons::render_svg_data(&svg, s as u32, s as u32))
                        }),
                    };
                    match img {
                        Some(img) => c.draw_cover(&img, x, mid - s / 2, s, s, 0.0, 1.0),
                        None => {
                            let name = if t.title.is_empty() { &t.icon_name } else { &t.title };
                            c.rounded_rect(x, mid - s / 2, s, s, s as f32 * 0.24, tile_color(name));
                            let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_else(|| "?".into());
                            self.tile_text.draw_centered(c, x + s / 2, mid, &initial, TILE_TEXT);
                        }
                    }
                    hits.push((x - 3, x + s + 3, Hit::Tray(i)));
                }
                Some((x0, right))
            }
            "keyboard" => {
                let name = sys.kb.as_ref()?;
                let label = if name.is_empty() { "??".to_string() } else { name.split('(').next().unwrap_or(name).to_uppercase() };
                let x = right - self.text.width(&label);
                self.text.draw(c, x, mid, &label, if hot(Hit::Keyboard) { with_alpha(accent, 0.7) } else { accent });
                hits.push((x - 6, right + 6, Hit::Keyboard));
                Some((x, right))
            }
            "system" => {
                if self.style.system_icon == "none" {
                    return None;
                }
                let px = if self.style.system_icon_size > 0 { self.style.system_icon_size } else { (self.style.item_icon * 115 + 50) / 100 }; // the logo is drawn 15% larger than the other icons
                let iw = self.ensure_system_icon(px);
                let x = right - iw;
                self.draw_system_icon(c, x, mid, px);
                hits.push((x - 3, right + 3, Hit::System));
                Some((x, right))
            }
            "workspaces" => {
                let accent = self.style.ws_color.unwrap_or(self.pal.primary);
                let shown: Vec<(usize, bool, bool)> = state
                    .workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, ws)| (i, i == state.active, !ws.windows.is_empty()))
                    .filter(|(_, active, occupied)| !(self.style.ws_occupied_only && !active && !occupied))
                    .collect();
                if shown.is_empty() {
                    return None;
                }
                // how far each indicator advances, and the trailing gap that is not part of the item
                let advance = |this: &Self, i: usize, active: bool, occupied: bool| -> i32 {
                    match this.style.ws_style {
                        WsStyle::Dots => (if active { 26 } else if occupied { 10 } else { 8 }) + 8,
                        WsStyle::Halo => (if active { 28 } else { 10 }) + 10 + 4,
                        WsStyle::Numbers => (this.text.width(&(i + 1).to_string()) + 14).max(24) + 4,
                    }
                };
                let trail = if self.style.ws_style == WsStyle::Dots { 8 } else { 4 };
                let total: i32 = shown.iter().map(|(i, a, o)| advance(self, *i, *a, *o)).sum::<i32>() - trail;
                let mut x = right - total;
                let x0 = x;
                for (i, active, occupied) in shown {
                    match self.style.ws_style {
                        WsStyle::Dots => {
                            let (pw, ph, color) = if active {
                                (26, 10, accent)
                            } else if occupied {
                                (10, 10, self.ws_inactive.unwrap_or(self.pal.on_surface_variant))
                            } else {
                                (8, 8, self.ws_inactive.unwrap_or(self.pal.outline))
                            };
                            c.pill(x, mid - ph / 2, pw, ph, color);
                            hits.push((x - 4, x + pw + 4, Hit::Workspace(i)));
                        }
                        WsStyle::Halo => {
                            // a filled pill (wide when current) inside a soft ring of the same colour
                            let (pw, ph) = if active { (28, 10) } else { (10, 10) };
                            let pad = 5;
                            let tone = if active { accent } else { self.ws_inactive.unwrap_or(accent) };
                            let col = if active || occupied { tone } else { with_alpha(tone, 0.45) };
                            c.pill(x, mid - ph / 2 - pad, pw + 2 * pad, ph + 2 * pad, with_alpha(tone, 0.24));
                            c.pill(x + pad, mid - ph / 2, pw, ph, col);
                            hits.push((x - 2, x + pw + 2 * pad + 2, Hit::Workspace(i)));
                        }
                        WsStyle::Numbers => {
                            let label = (i + 1).to_string();
                            let bw = (self.text.width(&label) + 14).max(24);
                            if active {
                                c.rounded_rect(x, mid - 12, bw, 24, 8.0, accent);
                            } else if hover == Some(Hit::Workspace(i)) {
                                c.rounded_rect(x, mid - 12, bw, 24, 8.0, with_alpha(self.pal.on_surface, 0.12));
                            }
                            let col = if active { self.pal.on_primary } else if let Some(b) = self.ws_inactive { b } else if occupied { self.pal.on_surface } else { self.pal.outline };
                            self.text.draw_centered(c, x + bw / 2, mid, &label, col);
                            hits.push((x - 2, x + bw + 2, Hit::Workspace(i)));
                        }
                    }
                    x += advance(self, i, active, occupied);
                }
                Some((x0, right))
            }
            "minimized" => {
                // only the windows minimized from the current workspace
                let mine: Vec<(usize, &vesper_ipc::WindowInfo)> = state.minimized.iter().enumerate().filter(|(i, _)| state.minimized_ws.get(*i).is_none_or(|w| *w == state.active)).collect();
                let s = self.icon_size;
                let cap = ((self.avail / 3) / (s + ICON_GAP)).max(3) as usize; // the rest stay reachable with Mod+Shift+M
                let mine: Vec<_> = mine.into_iter().take(cap).collect();
                if mine.is_empty() {
                    return None;
                }
                let n = mine.len() as i32;
                let total = n * s + (n - 1) * ICON_GAP;
                let x0 = right - total;
                let y = mid - s / 2;
                for (k, (i, win)) in mine.iter().enumerate() {
                    let (i, win) = (*i, *win);
                    let ix = x0 + k as i32 * (s + ICON_GAP);
                    if hover == Some(Hit::Restore(i)) {
                        c.rounded_rect(ix - 5, y - 4, s + 10, s + 8, 8.0, with_alpha(self.pal.on_surface, 0.15));
                    }
                    let app = self.apps.find(&win.app_id);
                    let icon_name = app.map(|a| a.icon.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| win.app_id.clone());
                    match self.icons.get(&icon_name, s) {
                        Some(img) => c.image(ix, y, &img),
                        None => {
                            let name = app.map(|a| a.name.as_str()).filter(|n| !n.is_empty()).unwrap_or(&win.app_id);
                            c.rounded_rect(ix, y, s, s, s as f32 * 0.24, tile_color(name));
                            let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_else(|| "?".into());
                            self.tile_text.draw_centered(c, ix + s / 2, mid, &initial, TILE_TEXT);
                        }
                    }
                    hits.push((ix - 5, ix + s + 5, Hit::Restore(i)));
                }
                Some((x0, right))
            }
            "brightness" => {
                let b = sys.brightness?;
                let glyph = if b < 34 { "\u{f00de}" } else if b < 67 { "\u{f00df}" } else { "\u{f00e0}" };
                let x = right - gt.width(glyph);
                gt.draw(c, x, mid, glyph, if hot(Hit::Brightness) { with_alpha(accent, 0.75) } else { accent });
                hits.push((x - 6, right + 6, Hit::Brightness));
                Some((x, right))
            }
            "clock" => {
                // the time in the normal size, the date after it smaller and dimmer
                match clock.split_once('\u{1}') {
                    Some((time, date)) => {
                        let small = self.text.resized(self.text.size * 0.8);
                        let dx = right - small.width(date);
                        small.draw(c, dx, mid, date, self.pal.on_surface_variant);
                        let x = dx - 8 - self.text.width(time);
                        self.text.draw(c, x, mid, time, self.pal.on_surface);
                        Some((x, right))
                    }
                    None => {
                        let x = right - self.text.width(clock);
                        self.text.draw(c, x, mid, clock, self.pal.on_surface);
                        Some((x, right))
                    }
                }
            }
            "battery" => {
                let Battery { percent, charging } = sys.battery?;
                if self.style.status_ios {
                    let bw = icons::battery_width(self.style.item_icon);
                    let x = right - bw;
                    // phone colours: the accent colour for the charge bar normally, green while charging, red at 20% or less
                    let fill: Rgba = if charging { [48, 209, 88, 255] } else if percent <= 20 { [255, 69, 58, 255] } else { accent };
                    let body = with_alpha(self.pal.on_surface, 0.55);
                    let ink = self.pal.surface;
                    let key = format!("bat:{}:{percent}:{charging}:{fill:?}:{body:?}:{ink:?}", self.style.item_icon);
                    let bar_h = self.style.item_icon;
                    let img = self.svg_cache.entry(key).or_insert_with(|| icons::battery(bar_h, percent, &icons::BatteryLook { fill, body, ink, charging })).clone();
                    c.image(x, mid - img.h / 2, &img);
                    Some((x, right))
                } else {
                    let glyph = match percent {
                        0..=12 => "\u{f244}",
                        13..=37 => "\u{f243}",
                        38..=62 => "\u{f242}",
                        63..=87 => "\u{f241}",
                        _ => "\u{f240}",
                    };
                    let color = if charging { GREEN } else if percent <= 15 { self.pal.error } else { accent };
                    let label = format!("{percent}%");
                    let mut x = right - self.text.width(&label);
                    self.text.draw(c, x, mid, &label, color);
                    x -= gt.width(glyph) + 12;
                    gt.draw(c, x, mid, glyph, color);
                    Some((x, right))
                }
            }
            "network" => {
                let (glyph, color) = match sys.net {
                    Net::Wifi => ("\u{f1eb}", accent),
                    Net::Ethernet => ("\u{f0200}", accent),
                    Net::Offline => ("\u{f05aa}", with_alpha(accent, 0.45)),
                };
                let x;
                if self.style.status_ios && sys.net != Net::Ethernet {
                    x = right - icons::wifi_width(self.style.item_icon);
                    let strength = if sys.net == Net::Wifi { 3 } else { 0 };
                    let muted = with_alpha(accent, 0.45); // an offline wifi looks like the muted speaker
                    let (on, off, bar_h) = if sys.net == Net::Wifi { (accent, with_alpha(accent, 0.30), self.style.item_icon) } else { (muted, muted, self.style.item_icon) };
                    let key = format!("wifi:{bar_h}:{strength}:{on:?}");
                    let img = self.svg_cache.entry(key).or_insert_with(|| icons::wifi(bar_h, strength, on, off)).clone();
                    c.image(x, mid - img.h / 2, &img);
                } else {
                    x = right - gt.width(glyph);
                    gt.draw(c, x, mid, glyph, color);
                }
                Some((x, right))
            }
            "volume" => {
                let Volume { percent, muted } = sys.volume?;
                // just the speaker: waves for the level, a cross when muted
                let color = if muted { with_alpha(accent, 0.45) } else { accent };
                let bar_h = self.style.item_icon;
                let key = format!("vol:{bar_h}:{}:{muted}:{color:?}", if muted || percent == 0 { 0 } else if percent < 34 { 1 } else if percent < 67 { 2 } else { 3 });
                let img = self.svg_cache.entry(key).or_insert_with(|| icons::volume(bar_h, percent, muted, color)).clone();
                let x = right - img.w;
                c.image(x, mid - img.h / 2, &img);
                hits.push((x - 6, right + 6, Hit::Volume));
                Some((x, right))
            }
            _ => None,
        }
    }
}

/// Width and height of the part of `img` that is not transparent.
fn alpha_extent(img: &Image) -> (i32, i32) {
    match alpha_bounds(img) {
        Some((x0, y0, x1, y1)) => (x1 - x0, y1 - y0),
        None => (0, 0),
    }
}

fn alpha_bounds(img: &Image) -> Option<(i32, i32, i32, i32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (img.w, img.h, -1, -1);
    for y in 0..img.h {
        for x in 0..img.w {
            if img.data[((y * img.w + x) * 4 + 3) as usize] > 8 {
                x0 = x0.min(x);
                x1 = x1.max(x + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x1 > 0).then_some((x0, y0, x1, y1))
}

/// `img` without its empty margins.
fn trim(img: &Image) -> Image {
    let Some((x0, y0, x1, y1)) = alpha_bounds(img) else { return img.clone() };
    let (w, h) = (x1 - x0, y1 - y0);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        let start = ((y * img.w + x0) * 4) as usize;
        data.extend_from_slice(&img.data[start..start + (w * 4) as usize]);
    }
    Image { w, h, data }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{Battery, Net, Volume};
    use vesper_ipc::{WindowInfo, Workspace};

    fn render(style_edit: impl Fn(&mut vesper_config::Bar), w: u32) -> Vec<u8> {
        let mut cfg = vesper_config::Bar::default();
        style_edit(&mut cfg);
        let mut r = Renderer::new(&cfg.font, cfg.font_size, cfg.height, cfg.icon_size);
        r.style = Style::from_config(&cfg);
        let win = |a: &str| WindowInfo { app_id: a.into(), title: a.into() };
        let state = State {
            workspaces: vec![Workspace { windows: vec![win("kitty")] }, Workspace::default(), Workspace { windows: vec![win("firefox")] }],
            active: 0,
            minimized: vec![win("firefox"), win("kitty")],
            minimized_ws: vec![0, 1],
            ..Default::default()
        };
        let sys = Sys {
            plugin: Default::default(),
            mic: Some(true),
            kb: Some("us".into()),
            media: None,
            tray: Vec::new(),
            stats: None,
            brightness: Some(60),
            notif: crate::sys::Notif { unread: 3, dnd: false },
            volume: Some(Volume { percent: 60, muted: false }),
            net: Net::Wifi,
            battery: Some(Battery { percent: 64, charging: false }),
            drives: Vec::new(),
            hist: Default::default(),
        };
        let h = r.style.surface_height() as u32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        r.draw(&mut px, w, h, &state, "Mon 16 Nov  14:05", &sys, None);
        px
    }

    fn write_png(path: &std::ffi::OsStr, w: u32, rows: &[Vec<u8>]) {
        let h = rows[0].len() as u32 / 4 / w;
        let mut rgb = Vec::new();
        for px in rows.iter().flat_map(|r| r.chunks_exact(4)) {
            let a = px[3] as u32;
            for c in [px[2], px[1], px[0]] {
                rgb.push((c as u32 + 30 * (255 - a) / 255) as u8);
            }
        }
        let mut enc = png::Encoder::new(std::fs::File::create(path).unwrap(), w, h * rows.len() as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }

    /// Writes bars with the default layout and with items moved around when VESPER_BAR_OUT names a file.
    #[test]
    fn write_bars_if_asked() {
        let Some(path) = std::env::var_os("VESPER_BAR_OUT") else { return };
        let w = 1200;
        let a = render(|_| {}, w);
        let b = render(
            |c| {
                c.left_items = vec!["workspaces".into(), "clock".into()];
                c.middle_items = vec!["system".into(), "volume".into(), "minimized".into()];
                c.right_items = vec!["bell".into(), "battery".into(), "network".into()];
            },
            w,
        );
        write_png(&path, w, &[a, b]);
    }
}

//! Software drawing into a premultiplied ARGB8888 (little-endian BGRA) buffer, the format `wl_shm` calls
//! `Argb8888`. Colours passed in are straight (non-premultiplied) RGBA.

pub mod backdrop;

pub type Rgba = [u8; 4];

/// Premultiplied BGRA bytes for `c`, with its alpha scaled by `cov` (0..=255).
pub fn premul(c: Rgba, cov: u8) -> [u8; 4] {
    let a = c[3] as u32 * cov as u32 / 255;
    [(c[2] as u32 * a / 255) as u8, (c[1] as u32 * a / 255) as u8, (c[0] as u32 * a / 255) as u8, a as u8]
}

pub struct Canvas<'a> {
    pub px: &'a mut [u8],
    pub w: i32,
    pub h: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u8], w: u32, h: u32) -> Self {
        Self { px, w: w as i32, h: h as i32 }
    }

    /// Replace the whole picture with `src` (premultiplied BGRA, same size); ignored if the size differs.
    pub fn copy_pixels(&mut self, src: &[u8]) {
        if src.len() == self.px.len() {
            self.px.copy_from_slice(src);
        }
    }

    pub fn clear(&mut self, c: Rgba) {
        let p = premul(c, 255);
        for px in self.px.chunks_exact_mut(4) {
            px.copy_from_slice(&p);
        }
    }

    /// Source-over blend of `color` at coverage `cov` onto the pixel at (x, y).
    pub fn blend(&mut self, x: i32, y: i32, color: Rgba, cov: u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        self.blend_premul(i, premul(color, cov));
    }

    fn blend_premul(&mut self, i: usize, src: [u8; 4]) {
        let inv = 255 - src[3] as u32;
        for k in 0..4 {
            self.px[i + k] = (src[k] as u32 + self.px[i + k] as u32 * inv / 255).min(255) as u8;
        }
    }

    /// Fill an axis-aligned rectangle (blended, so a translucent colour works).
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        for yy in y.max(0)..(y + h).min(self.h) {
            self.span(yy, x, x + w, color);
        }
    }

    /// One horizontal run of pixels [x0, x1) on row `y`, all the same colour (fast: no per-pixel maths).
    pub fn span(&mut self, y: i32, x0: i32, x1: i32, color: Rgba) {
        if y < 0 || y >= self.h {
            return;
        }
        let (x0, x1) = (x0.max(0), x1.min(self.w));
        if x0 >= x1 {
            return;
        }
        let src = premul(color, 255);
        let row = (y * self.w * 4) as usize;
        let slice = &mut self.px[row + (x0 * 4) as usize..row + (x1 * 4) as usize];
        if src[3] == 255 {
            for px in slice.chunks_exact_mut(4) {
                px.copy_from_slice(&src);
            }
        } else {
            let inv = 255 - src[3] as u32;
            for px in slice.chunks_exact_mut(4) {
                for k in 0..4 {
                    px[k] = (src[k] as u32 + px[k] as u32 * inv / 255).min(255) as u8;
                }
            }
        }
    }

    /// Rounded rectangle with corner radius `r`, edges anti-aliased.
    /// Fill one whole row with a single colour (fast gradients and backgrounds).
    pub fn fill_row(&mut self, y: i32, c: Rgba) {
        if y < 0 || y >= self.h {
            return;
        }
        let p = premul(c, 255);
        let start = (y * self.w * 4) as usize;
        for px in self.px[start..start + (self.w * 4) as usize].chunks_exact_mut(4) {
            px.copy_from_slice(&p);
        }
    }

    pub fn rounded_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, color: Rgba) {
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
        let rc = r.ceil() as i32 + 1;
        for py in y.max(0)..(y + h).min(self.h) {
            let fy = py as f32 + 0.5;
            let corner_row = fy < y as f32 + r + 1.0 || fy > (y + h) as f32 - r - 1.0;
            if !corner_row || w <= 2 * rc {
                if !corner_row {
                    self.span(py, x, x + w, color);
                    continue;
                }
            }
            // only the corner squares need coverage maths; the stretch between them is solid
            let (left_end, right_start) = if w > 2 * rc { (x + rc, x + w - rc) } else { (x + w, x + w) };
            for px in (x.max(0)..left_end.min(self.w)).chain(right_start.max(x).max(0)..(x + w).min(self.w)) {
                let fx = px as f32 + 0.5;
                let cx = fx.clamp(x as f32 + r, (x + w) as f32 - r);
                let cy = fy.clamp(y as f32 + r, (y + h) as f32 - r);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, color, (cov * 255.0) as u8);
                }
            }
            if left_end < right_start {
                self.span(py, left_end, right_start, color);
            }
        }
    }

    /// Fully rounded ends.
    pub fn pill(&mut self, x: i32, y: i32, w: i32, h: i32, color: Rgba) {
        self.rounded_rect(x, y, w, h, h as f32 / 2.0, color);
    }

    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, color: Rgba) {
        self.rounded_rect(cx - r, cy - r, r * 2, r * 2, r as f32, color);
    }

    /// Draw a premultiplied BGRA image (e.g. an icon) with its top-left at (x, y).
    pub fn image(&mut self, x: i32, y: i32, img: &Image) {
        for iy in 0..img.h {
            for ix in 0..img.w {
                let (px, py) = (x + ix, y + iy);
                if px < 0 || py < 0 || px >= self.w || py >= self.h {
                    continue;
                }
                let s = ((iy * img.w + ix) * 4) as usize;
                let src = [img.data[s], img.data[s + 1], img.data[s + 2], img.data[s + 3]];
                if src[3] != 0 {
                    self.blend_premul(((py * self.w + px) * 4) as usize, src);
                }
            }
        }
    }
}

/// A premultiplied BGRA bitmap.
#[derive(Clone)]
pub struct Image {
    pub w: i32,
    pub h: i32,
    pub data: Vec<u8>,
}

impl Image {
    /// From straight RGBA8 pixels (what `png`/`tiny-skia` give you after un-premultiplying is not needed:
    /// pass `premultiplied = true` for tiny-skia's own pixmaps).
    pub fn from_rgba(w: i32, h: i32, rgba: &[u8], premultiplied: bool) -> Self {
        let mut data = Vec::with_capacity(rgba.len());
        for p in rgba.chunks_exact(4) {
            if premultiplied {
                data.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            } else {
                data.extend_from_slice(&premul([p[0], p[1], p[2], p[3]], 255));
            }
        }
        Self { w, h, data }
    }
}

#[derive(Clone)]
pub struct Text {
    font: Option<ab_glyph::FontArc>,
    pub size: f32,
}

impl Text {
    /// Load `family` from the system fonts, falling back to any sans-serif face, then any face at all.
    pub fn new(family: &str, size: f32) -> Self {
        Self { font: load_font(family), size }
    }

    /// The same font at another size, without loading it again.
    pub fn resized(&self, size: f32) -> Self {
        Self { font: self.font.clone(), size }
    }

    /// ab_glyph sizes by line height; convert so `size` means em size in pixels, like other toolkits.
    fn scale(&self) -> f32 {
        use ab_glyph::Font;
        match &self.font {
            Some(f) => match f.units_per_em() {
                Some(upm) if upm > 0.0 => self.size * f.height_unscaled() / upm,
                _ => self.size,
            },
            None => self.size,
        }
    }

    pub fn width(&self, s: &str) -> i32 {
        use ab_glyph::{Font, ScaleFont};
        let Some(f) = &self.font else { return 0 };
        let f = f.as_scaled(self.scale());
        s.chars().map(|ch| f.h_advance(f.glyph_id(ch))).sum::<f32>().ceil() as i32
    }

    /// Truncate with an ellipsis so the text fits in `max` pixels.
    pub fn fit(&self, s: &str, max: i32) -> String {
        if self.width(s) <= max {
            return s.to_owned();
        }
        let mut out = s.to_owned();
        while !out.is_empty() && self.width(&format!("{out}…")) > max {
            out.pop();
        }
        if out.is_empty() { out } else { format!("{out}…") }
    }

    /// Draw `s` with its left edge at `x` and its vertical centre at `mid`.
    pub fn draw(&self, c: &mut Canvas, x: i32, mid: i32, s: &str, color: Rgba) {
        use ab_glyph::{point, Font, ScaleFont};
        let Some(font) = &self.font else { return };
        let scale = self.scale();
        let scaled = font.as_scaled(scale);
        let baseline = mid as f32 + (scaled.ascent() + scaled.descent()) / 2.0;
        let mut pen = x as f32;
        for ch in s.chars() {
            let id = scaled.glyph_id(ch);
            let glyph = id.with_scale_and_position(scale, point(pen, baseline.round()));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                let (ox, oy) = (b.min.x as i32, b.min.y as i32);
                outlined.draw(|gx, gy, cov| {
                    let a = (cov * 255.0) as u8;
                    if a > 0 {
                        c.blend(ox + gx as i32, oy + gy as i32, color, a);
                    }
                });
            }
            pen += scaled.h_advance(id);
        }
    }

    /// Draw `s` centred on (cx, cy).
    pub fn draw_centered(&self, c: &mut Canvas, cx: i32, cy: i32, s: &str, color: Rgba) {
        self.draw(c, cx - self.width(s) / 2, cy, s, color);
    }
}

fn parse(data: Vec<u8>, index: u32) -> Option<ab_glyph::FontArc> {
    ab_glyph::FontVec::try_from_vec_and_index(data, index).ok().map(ab_glyph::FontArc::from)
}

/// Where we remember which file a font family resolved to, so later starts skip scanning every font on disk.
fn cache_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    Some(base.join("vesper/fonts.tsv"))
}

fn cached(family: &str) -> Option<ab_glyph::FontArc> {
    let text = std::fs::read_to_string(cache_file()?).ok()?;
    text.lines().find_map(|line| {
        let mut f = line.split('\t');
        (f.next()? == family).then_some(())?;
        let path = f.next()?;
        let index = f.next()?.parse().ok()?;
        parse(std::fs::read(path).ok()?, index) // a vanished file just means a cache miss
    })
}

fn remember(family: &str, path: &std::path::Path, index: u32) {
    let Some(file) = cache_file() else { return };
    let mut lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split('\t').next() != Some(family))
        .map(String::from)
        .collect();
    lines.push(format!("{family}\t{}\t{index}", path.display()));
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, lines.join("\n") + "\n");
}

fn load_font(family: &str) -> Option<ab_glyph::FontArc> {
    if let Some(font) = cached(family) {
        return Some(font);
    }
    // Slow path: index every installed font (hundreds of MB on some systems). Done once, then cached.
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family), fontdb::Family::SansSerif],
        ..Default::default()
    };
    let id = db.query(&query).or_else(|| db.faces().next().map(|f| f.id))?;
    let face = db.face(id)?;
    tracing::info!("font {family:?} resolved to {:?}", face.families.first());
    if let fontdb::Source::File(path) = &face.source {
        remember(family, path, face.index);
    }
    db.with_face_data(id, |data, index| parse(data.to_vec(), index))?
}

/// Coverage (0..=1) of a rounded rectangle at pixel centre (fx, fy).
fn rr_coverage(fx: f32, fy: f32, x: i32, y: i32, w: i32, h: i32, r: f32) -> f32 {
    let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
    let cx = fx.clamp(x as f32 + r, (x + w) as f32 - r);
    let cy = fy.clamp(y as f32 + r, (y + h) as f32 - r);
    let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

impl Canvas<'_> {
    /// Draw `img` scaled to *cover* the `w`x`h` rectangle at (x, y) (cropped from the centre), with rounded corners.
    /// `opacity` dims it (1.0 = as is). Bilinear sampling, so it also looks fine when scaled down.
    pub fn draw_cover(&mut self, img: &Image, x: i32, y: i32, w: i32, h: i32, radius: f32, opacity: f32) {
        if w <= 0 || h <= 0 || img.w <= 0 || img.h <= 0 {
            return;
        }
        let s = (w as f32 / img.w as f32).max(h as f32 / img.h as f32);
        let (off_x, off_y) = ((img.w as f32 - w as f32 / s) / 2.0, (img.h as f32 - h as f32 / s) / 2.0);
        let (maxx, maxy) = (img.w - 1, img.h - 1);
        let px = |ix: i32, iy: i32, k: usize| -> f32 {
            img.data[((iy.clamp(0, maxy) * img.w + ix.clamp(0, maxx)) * 4) as usize + k] as f32
        };
        for dy in 0..h {
            let cy = y + dy;
            if cy < 0 || cy >= self.h {
                continue;
            }
            let sy = (dy as f32 + 0.5) / s + off_y - 0.5;
            let (iy, fy) = (sy.floor() as i32, sy - sy.floor());
            for dx in 0..w {
                let cx = x + dx;
                if cx < 0 || cx >= self.w {
                    continue;
                }
                let corner = (dx as f32) < radius + 1.0 || (dx as f32) > w as f32 - radius - 1.0;
                let corner = corner && ((dy as f32) < radius + 1.0 || (dy as f32) > h as f32 - radius - 1.0);
                let cov = if corner { rr_coverage(cx as f32 + 0.5, cy as f32 + 0.5, x, y, w, h, radius) * opacity } else { opacity };
                if cov <= 0.0 {
                    continue;
                }
                let sx = (dx as f32 + 0.5) / s + off_x - 0.5;
                let (ix, fx) = (sx.floor() as i32, sx - sx.floor());
                let mut src = [0u8; 4];
                for k in 0..4 {
                    let top = px(ix, iy, k) * (1.0 - fx) + px(ix + 1, iy, k) * fx;
                    let bot = px(ix, iy + 1, k) * (1.0 - fx) + px(ix + 1, iy + 1, k) * fx;
                    src[k] = ((top * (1.0 - fy) + bot * fy) * cov) as u8;
                }
                self.blend_premul(((cy * self.w + cx) * 4) as usize, src);
            }
        }
    }

    /// A rounded rectangle's outline, `thickness` pixels wide, drawn inside the given rectangle.
    pub fn rounded_outline(&mut self, x: i32, y: i32, w: i32, h: i32, radius: f32, thickness: f32, color: Rgba) {
        let band = radius.ceil() as i32 + thickness.ceil() as i32 + 2;
        for py in y.max(0)..(y + h).min(self.h) {
            let edge_row = py < y + band || py >= y + h - band;
            let xs = x.max(0)..(x + w).min(self.w);
            // away from the top and bottom, only the strips near the left and right edges can be on the outline
            let cols: Vec<i32> = if edge_row {
                xs.collect()
            } else {
                (x.max(0)..(x + band).min(self.w)).chain((x + w - band).max(x).max(0)..(x + w).min(self.w)).collect()
            };
            for px in cols {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let outer = rr_coverage(fx, fy, x, y, w, h, radius);
                let inner = rr_coverage(
                    fx,
                    fy,
                    x + thickness as i32,
                    y + thickness as i32,
                    w - 2 * thickness as i32,
                    h - 2 * thickness as i32,
                    (radius - thickness).max(0.0),
                );
                let cov = (outer - inner).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// A horizontal line centred on `cx` that fades out towards both ends.
    pub fn glow_line(&mut self, cx: i32, y: i32, half_width: i32, thickness: i32, color: Rgba) {
        for dx in -half_width..=half_width {
            let t = 1.0 - (dx as f32 / half_width as f32).abs();
            let a = (t.powf(0.8) * 255.0) as u8;
            for dy in 0..thickness {
                self.blend(cx + dx, y + dy, color, a);
            }
        }
    }
}

impl Text {
    /// Width of `s` with `spacing` extra pixels between letters.
    pub fn width_spaced(&self, s: &str, spacing: i32) -> i32 {
        let n = s.chars().count() as i32;
        s.chars().map(|c| self.width(&c.to_string())).sum::<i32>() + spacing * (n - 1).max(0)
    }

    /// Draw `s` letter by letter with extra spacing, left edge at `x`, vertical centre at `mid`.
    pub fn draw_spaced(&self, c: &mut Canvas, x: i32, mid: i32, s: &str, color: Rgba, spacing: i32) {
        let mut pen = x;
        for ch in s.chars() {
            let t = ch.to_string();
            self.draw(c, pen, mid, &t, color);
            pen += self.width(&t) + spacing;
        }
    }

    pub fn draw_centered_spaced(&self, c: &mut Canvas, cx: i32, mid: i32, s: &str, color: Rgba, spacing: i32) {
        self.draw_spaced(c, cx - self.width_spaced(s, spacing) / 2, mid, s, color, spacing);
    }
}

#[cfg(test)]
mod fast_path_tests {
    use super::*;

    /// The straightforward per-pixel version, to check the fast one against.
    fn reference(px: &mut [u8], cw: i32, ch: i32, x: i32, y: i32, w: i32, h: i32, r: f32, color: Rgba) {
        let mut c = Canvas { px, w: cw, h: ch };
        let r = r.min(w as f32 / 2.0).min(h as f32 / 2.0);
        for py in y..y + h {
            for qx in x..x + w {
                let fx = qx as f32 + 0.5;
                let fy = py as f32 + 0.5;
                let cx = fx.clamp(x as f32 + r, (x + w) as f32 - r);
                let cy = fy.clamp(y as f32 + r, (y + h) as f32 - r);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    c.blend(qx, py, color, (cov * 255.0) as u8);
                }
            }
        }
    }

    #[test]
    fn rounded_rect_fast_path_matches_reference() {
        for (w, h, r) in [(60, 40, 12.0), (30, 30, 15.0), (50, 8, 4.0), (9, 9, 20.0), (64, 64, 1.5)] {
            for color in [[200u8, 100, 50, 255], [10, 200, 90, 128]] {
                let (cw, ch) = (120, 90);
                let mut a = vec![20u8; (cw * ch * 4) as usize];
                let mut b = a.clone();
                Canvas::new(&mut a, cw as u32, ch as u32).rounded_rect(7, 5, w, h, r, color);
                reference(&mut b, cw, ch, 7, 5, w, h, r, color);
                assert_eq!(a, b, "{w}x{h} r={r}");
            }
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    #[test]
    fn big_rounded_rects_are_cheap() {
        let (w, h) = (2080, 1440);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let mut c = Canvas::new(&mut buf, w as u32, h as u32);
        let t = std::time::Instant::now();
        for _ in 0..20 {
            c.rounded_rect(300, 100, 1600, 1200, 28.0, [30, 30, 40, 255]);
            c.rounded_outline(300, 100, 1600, 1200, 28.0, 2.0, [200, 200, 255, 255]);
        }
        let per = t.elapsed() / 20;
        println!("full-window card + outline: {per:?}");
        assert!(per < std::time::Duration::from_millis(15), "too slow: {per:?}");
    }
}

impl Text {
    /// A font from the bytes of a TrueType file (for bundled fonts such as the emoji font).
    pub fn from_font_data(data: Vec<u8>, size: f32) -> Option<Self> {
        Some(Self { font: parse(data, 0), size }).filter(|t| t.font.is_some())
    }

    /// Does the font have a glyph for `c`?
    pub fn has_glyph(&self, c: char) -> bool {
        use ab_glyph::Font;
        self.font.as_ref().is_some_and(|f| f.glyph_id(c).0 != 0)
    }
}

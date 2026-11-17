//! Status icons drawn from small SVGs rendered at the exact pixel size, modelled on a phone's status bar: a battery whose
//! charge shows as a bright bar over a grey body with the number inside it, and a wifi fan of round-ended arcs.

use vesper_gfx::{Image, Rgba};

fn css(c: Rgba) -> String {
    format!("rgb({},{},{})", c[0], c[1], c[2])
}

fn op(c: Rgba) -> f32 {
    c[3] as f32 / 255.0
}

fn empty() -> Image {
    Image { w: 0, h: 0, data: Vec::new() }
}

/// Height of the battery and wifi icons for an icon size of `bar_h` pixels (named so for history).
pub fn icon_height(bar_h: i32) -> i32 {
    ((bar_h as f32 * 0.7).round() as i32).max(8)
}

fn body_w(h: f32) -> f32 {
    h * 1.83
}

/// Total width of the battery icon including its contact.
pub fn battery_width(bar_h: i32) -> i32 {
    let h = icon_height(bar_h) as f32;
    (body_w(h) + h * 0.05 + h * 0.17).round() as i32
}

/// How the battery is coloured.
pub struct BatteryLook {
    /// The charge bar.
    pub fill: Rgba,
    /// The part of the body that is empty (and the contact).
    pub body: Rgba,
    /// The number.
    pub ink: Rgba,
    pub charging: bool,
}

/// Stroke outlines (centre lines) of the digits 0-9 on a 6 x 10 grid, with round ends; drawn thick they look like a bold
/// rounded phone font.
const DIGITS: [&str; 10] = [
    "M3 0.9 C1.4 0.9 0.9 2.6 0.9 5 C0.9 7.4 1.4 9.1 3 9.1 C4.6 9.1 5.1 7.4 5.1 5 C5.1 2.6 4.6 0.9 3 0.9 Z",
    "M1.2 2.9 L3.5 1 L3.5 9.1",
    "M0.9 3.2 C1 1.8 1.9 0.9 3.1 0.9 C4.4 0.9 5.1 1.8 5.1 3 C5.1 4.2 4.4 5 3.3 6 L0.9 9.1 L5.2 9.1",
    "M1 2 C1.6 1.2 2.3 0.9 3.1 0.9 C4.3 0.9 5 1.6 5 2.7 C5 3.9 4.2 4.6 2.9 4.6 C4.4 4.6 5.2 5.4 5.2 6.8 C5.2 8.3 4.3 9.1 3 9.1 C1.9 9.1 1.2 8.7 0.8 7.9",
    "M4.3 9.1 L4.3 1 L0.9 6.5 L5.3 6.5",
    "M4.9 1 L1.7 1 L1.3 4.5 C1.9 4 2.5 3.8 3.2 3.8 C4.6 3.8 5.3 4.8 5.3 6.4 C5.3 8.1 4.4 9.1 3 9.1 C2 9.1 1.2 8.6 0.8 7.8",
    "M4.8 2 C4.3 1.3 3.7 0.9 3 0.9 C1.6 0.9 0.9 2.6 0.9 5.5 C0.9 7.9 1.6 9.1 3 9.1 C4.4 9.1 5.2 8.1 5.2 6.5 C5.2 5.1 4.4 4.2 3 4.2 C2.1 4.2 1.2 4.7 0.9 5.7",
    "M0.9 1 L5.2 1 L2.5 9.1",
    "M3 4.6 C1.7 4.6 1 3.8 1 2.7 C1 1.6 1.8 0.9 3 0.9 C4.2 0.9 5 1.6 5 2.7 C5 3.8 4.3 4.6 3 4.6 C1.6 4.6 0.8 5.5 0.8 6.8 C0.8 8.2 1.6 9.1 3 9.1 C4.4 9.1 5.2 8.2 5.2 6.8 C5.2 5.5 4.4 4.6 3 4.6 Z",
    "",
];

/// SVG for a number drawn with the digit strokes: centred on (cx, cy), `dh` tall, stroke colour `ink`.
fn number_svg(text: &str, cx: f32, cy: f32, dh: f32, ink: Rgba) -> String {
    let s = dh / 10.0;
    let sx = s * 1.14; // a little wider than tall, like a rounded phone font
    let pitch = 6.9 * sx;
    let n = text.len() as f32;
    let total = (n - 1.0) * pitch + 6.0 * sx;
    let x0 = cx - total / 2.0;
    let y0 = cy - dh / 2.0;
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        let d = ch.to_digit(10).unwrap_or(0) as usize;
        let x = x0 + i as f32 * pitch;
        let path = if d == 9 { DIGITS[6] } else { DIGITS[d] };
        let rot = if d == 9 { format!(r#" rotate(180 3 5)"#) } else { String::new() };
        out.push_str(&format!(
            r#"<g transform="translate({x} {y0}) scale({sx} {s}){rot}"><path d="{path}" fill="none" stroke="{c}" stroke-opacity="{o}" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></g>"#,
            c = css(ink),
            o = op(ink),
        ));
    }
    out
}

/// The battery as a picture, `percent` full, with its number inside.
pub fn battery(bar_h: i32, percent: u32, look: &BatteryLook) -> Image {
    let h = icon_height(bar_h) as f32;
    let bw = body_w(h);
    let (gap, nw) = (h * 0.05, h * 0.17);
    let w = (bw + gap + nw).round() as i32;
    let r = h * 0.27;
    let level = percent.min(100) as f32 / 100.0;
    let fw = bw * level;
    // the number, and (while charging) a small bolt in front of it
    let label = percent.min(100).to_string();
    let dh = h * 0.55;
    // while charging only the bolt is shown, a little larger, with no number
    let digits_w = if look.charging { 0.0 } else { ((label.len() as f32 - 1.0) * 6.9 + 6.0) * (dh / 10.0) * 1.14 };
    let bolt_w = if look.charging { h * 0.46 } else { 0.0 };
    let bolt_gap = 0.0;
    let total = digits_w + bolt_w + bolt_gap;
    let left = bw / 2.0 - total / 2.0;
    let bolt = if look.charging {
        let (bx, by) = (left, h / 2.0);
        format!(
            r#"<path d="M {a} {b} L {c} {d} L {e} {d} L {f} {g} L {i} {j} L {k} {j} Z" fill="{ink}" fill-opacity="{io}"/>"#,
            a = bx + bolt_w * 0.62, b = by - h * 0.30,
            c = bx + bolt_w * 0.05, d = by + h * 0.03,
            e = bx + bolt_w * 0.45,
            f = bx + bolt_w * 0.30, g = by + h * 0.32,
            i = bx + bolt_w * 0.98, j = by - h * 0.06,
            k = bx + bolt_w * 0.58,
            ink = css(look.ink),
            io = op(look.ink),
        )
    } else {
        String::new()
    };
    let digits = if look.charging { String::new() } else { number_svg(&label, left + bolt_w + bolt_gap + digits_w / 2.0, h / 2.0, dh, look.ink) };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{hi}" viewBox="0 0 {w} {hi}">
<defs><clipPath id="c"><rect width="{bw}" height="{h}" rx="{r}"/></clipPath></defs>
<rect width="{bw}" height="{h}" rx="{r}" fill="{b}" fill-opacity="{bo}"/>
<g clip-path="url(#c)"><rect width="{fw}" height="{h}" fill="{f}" fill-opacity="{fo}"/></g>
<path d="M {nx} {ny} L {nx2} {ny} A {nr} {nr} 0 0 1 {nx2} {ny2} L {nx} {ny2} Z" fill="{b}" fill-opacity="{bo}"/>
{bolt}{digits}
</svg>"#,
        hi = h.round() as i32,
        b = css(look.body),
        bo = op(look.body),
        f = css(look.fill),
        fo = op(look.fill),
        nx = bw + gap,
        nx2 = bw + gap + nw * 0.35,
        ny = h * 0.33,
        ny2 = h * 0.67,
        nr = (h * 0.17) * 1.0,
    );
    vesper_apps::icons::render_svg_data(&svg, w as u32, h.round() as u32).unwrap_or_else(empty)
}

/// Width of the wifi symbol.
pub fn wifi_width(bar_h: i32) -> i32 {
    (icon_height(bar_h) as f32 * 1.36).round() as i32
}

/// The wifi fan: two round-ended arcs and a small rounded wedge, lit (`on`) or dim (`off`) by `strength` (0..=3).
pub fn wifi(bar_h: i32, strength: u32, on: Rgba, off: Rgba) -> Image {
    let h = icon_height(bar_h) as f32;
    let w = wifi_width(bar_h) as f32;
    let (cx, cy) = (w / 2.0, h);
    let half = 41.0_f32.to_radians();
    let (s, c) = (half.sin(), half.cos());
    let t = (h * 0.15).max(1.8); // arc thickness (never thinner than about two pixels)
    let col = |lit: bool| if lit { on } else { off };
    let arc = |r: f32, lit: bool| {
        let (x0, y0, x1, y1) = (cx - r * s, cy - r * c, cx + r * s, cy - r * c);
        let k = col(lit);
        format!(
            r#"<path d="M {x0} {y0} A {r} {r} 0 0 1 {x1} {y1}" fill="none" stroke="{c}" stroke-opacity="{o}" stroke-width="{t}" stroke-linecap="round"/>"#,
            c = css(k),
            o = op(k),
        )
    };
    // the wedge, shrunk by the rounding radius and drawn with a round-joined stroke of twice that width
    let cr = h * 0.06;
    let wr = h * 0.30 - cr;
    let apex_y = cy - cr / half.sin();
    let (a, b) = ((cx - wr * s, cy - wr * c), (cx + wr * s, cy - wr * c));
    let k = col(strength >= 1);
    let wedge = format!(
        r#"<path d="M {cx} {apex_y} L {} {} A {wr} {wr} 0 0 1 {} {} Z" fill="{c}" fill-opacity="{o}" stroke="{c}" stroke-opacity="{o}" stroke-width="{sw}" stroke-linejoin="round"/>"#,
        a.0,
        a.1,
        b.0,
        b.1,
        c = css(k),
        o = op(k),
        sw = cr * 2.0,
    );
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{}{}{wedge}</svg>"#,
        arc(h * 0.93 - t / 2.0 + t / 2.0 - 0.0, strength >= 3),
        arc(h * 0.56, strength >= 2),
    );
    vesper_apps::icons::render_svg_data(&svg, w as u32, h as u32).unwrap_or_else(empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_render_with_sensible_sizes() {
        let look = BatteryLook { fill: [255, 255, 255, 255], body: [255, 255, 255, 140], ink: [20, 24, 36, 255], charging: false };
        let b = battery(32, 25, &look);
        assert_eq!(b.w, battery_width(32));
        assert_eq!(b.h, icon_height(32));
        let w = wifi(32, 3, [255, 255, 255, 255], [255, 255, 255, 80]);
        assert_eq!(w.w, wifi_width(32));
        assert!(b.data.iter().any(|v| *v != 0) && w.data.iter().any(|v| *v != 0));
        // three digit numbers fit inside the body
        assert!(battery(32, 100, &look).w > 0);
    }

    /// Writes a sheet of samples when VESPER_ICON_OUT names a file (for looking at them by eye).
    #[test]
    fn write_samples_if_asked() {
        let Some(path) = std::env::var_os("VESPER_ICON_OUT") else { return };
        let bg = [21u8, 26, 39, 255];
        let (w, h) = (900, 420);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[bg[2], bg[1], bg[0], 255]);
        }
        let mut c = vesper_gfx::Canvas::new(&mut px, w as u32, h as u32);
        let white = [255, 255, 255, 255];
        let mut x = 20;
        for (pct, charging, fill) in [(25, false, [139, 123, 255, 255]), (100, false, [139, 123, 255, 255]), (68, true, [48, 209, 88, 255]), (12, false, [255, 69, 58, 255])] {
            let look = BatteryLook { fill, body: [255, 255, 255, 140], ink: bg, charging };
            let img = battery(200, pct, &look);
            c.image(x, 20, &img);
            x += img.w + 20;
        }
        let img = wifi(200, 3, white, [255, 255, 255, 80]);
        c.image(20, 200, &img);
        let img = wifi(200, 1, white, [255, 255, 255, 80]);
        c.image(400, 200, &img);
        // as small as on a real bar
        c.image(700, 220, &battery(32, 25, &BatteryLook { fill: white, body: [255, 255, 255, 140], ink: bg, charging: false }));
        c.image(700, 260, &wifi(32, 3, white, [255, 255, 255, 80]));
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            let mut rgba = px.clone();
            for p in rgba.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            wr.write_image_data(&rgba).unwrap();
        }
        std::fs::write(path, out).unwrap();
    }
}

/// Size of the speaker icon: a little taller than the battery so it reads at the same weight.
pub fn volume_height(bar_h: i32) -> i32 {
    ((icon_height(bar_h) as f32 * 1.30).round() as i32).max(12)
}

pub fn volume_width(bar_h: i32) -> i32 {
    (volume_height(bar_h) as f32 * 26.0 / 24.0).round() as i32
}

/// A speaker with sound waves: none (muted, with a cross), one, two or three waves by volume.
pub fn volume(bar_h: i32, percent: u32, muted: bool, color: Rgba) -> Image {
    let (w, h) = (volume_width(bar_h), volume_height(bar_h));
    let (c, o) = (css(color), op(color));
    let waves = if muted || percent == 0 {
        0
    } else if percent < 34 {
        1
    } else if percent < 67 {
        2
    } else {
        3
    };
    // the speaker: a rounded box that opens into a cone (24 x 24 grid, shifted left to leave room for the waves)
    let mut body = format!(
        r#"<path d="M 3.2 9.2 H 6.2 L 10.6 5.4 C 11.5 4.6 12.9 5.2 12.9 6.4 V 17.6 C 12.9 18.8 11.5 19.4 10.6 18.6 L 6.2 14.8 H 3.2 C 2.4 14.8 1.8 14.2 1.8 13.4 V 10.6 C 1.8 9.8 2.4 9.2 3.2 9.2 Z" fill="{c}" fill-opacity="{o}"/>"#
    );
    if muted || percent == 0 {
        body.push_str(&format!(
            r#"<path d="M 16 9.2 L 22 14.8 M 22 9.2 L 16 14.8" fill="none" stroke="{c}" stroke-opacity="{o}" stroke-width="2" stroke-linecap="round"/>"#
        ));
    }
    let (cx, cy) = (11.4_f32, 12.0_f32);
    let a = std::f32::consts::FRAC_PI_4 * 0.9;
    for (i, r) in [4.6_f32, 8.0, 11.4].iter().take(waves).enumerate() {
        let _ = i;
        let (dx, dy) = (r * a.cos(), r * a.sin());
        body.push_str(&format!(
            r#"<path d="M {} {} A {r} {r} 0 0 1 {} {}" fill="none" stroke="{c}" stroke-opacity="{o}" stroke-width="2" stroke-linecap="round"/>"#,
            cx + dx, cy - dy, cx + dx, cy + dy
        ));
    }
    let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 26 24">{body}</svg>"#);
    vesper_apps::icons::render_svg_data(&svg, w as u32, h as u32).unwrap_or_else(empty)
}

#[cfg(test)]
mod small_sheet {
    use super::*;

    /// Writes the icons at real bar size to VESPER_ICON_SMALL (view it enlarged).
    #[test]
    fn write_small_samples_if_asked() {
        let Some(path) = std::env::var_os("VESPER_ICON_SMALL") else { return };
        let bg = [21u8, 26, 39, 255];
        let (w, h) = (300, 40);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&[bg[2], bg[1], bg[0], 255]);
        }
        let mut c = vesper_gfx::Canvas::new(&mut px, w as u32, h as u32);
        let white = [235, 235, 245, 255];
        let accent = [139, 123, 255, 255];
        let mut x = 8;
        for (pct, muted) in [(0, true), (20, false), (50, false), (90, false)] {
            let img = volume(32, pct, muted, white);
            c.image(x, 20 - img.h / 2, &img);
            x += img.w + 8;
        }
        let img = wifi(32, 3, accent, [139, 123, 255, 80]);
        c.image(x, 20 - img.h / 2, &img);
        x += img.w + 10;
        let look = BatteryLook { fill: accent, body: [255, 255, 255, 140], ink: bg, charging: false };
        let img = battery(32, 25, &look);
        c.image(x, 20 - img.h / 2, &img);
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            let mut rgba = px.clone();
            for p in rgba.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            wr.write_image_data(&rgba).unwrap();
        }
        std::fs::write(path, out).unwrap();
    }
}

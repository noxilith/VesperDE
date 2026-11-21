//! The outlines the bar is drawn in (full strip, floating box, notch, islands) as distance fields, so every one gets a
//! smooth fill and an even border without special cases.

use vesper_gfx::{Canvas, Rgba};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// Plain rectangle.
    Rect { x: i32, y: i32, w: i32, h: i32 },
    /// Rectangle with all four corners rounded.
    Round { x: i32, y: i32, w: i32, h: i32, r: i32 },
    /// A tab hanging from the top edge (y = 0): rounded at the bottom, curving outwards into the top edge.
    Notch { x: i32, w: i32, h: i32, r: i32 },
}

fn box_sdf(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let (qx, qy) = ((px - cx).abs() - hx + r, (py - cy).abs() - hy + r);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

impl Shape {
    /// Distance used for the border.
    pub fn border_sdf(&self, px: f32, py: f32, border: f32) -> f32 {
        self.sdf(px, py, border)
    }

    /// How much of the border to draw at row `py`: the notch outline follows its curves right up to, but not along, the top edge.
    pub fn border_mask(&self, py: f32) -> f32 {
        match *self {
            Shape::Notch { .. } => (py - 1.0).clamp(0.0, 1.0),
            _ => 1.0,
        }
    }

    /// Signed distance from (px, py) to the outline: negative inside.
    pub fn sdf(&self, px: f32, py: f32, border: f32) -> f32 {
        match *self {
            Shape::Rect { x, y, w, h } => box_sdf(px, py, x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0, w as f32 / 2.0, h as f32 / 2.0, 0.0),
            Shape::Round { x, y, w, h, r } => {
                let r = (r as f32).min(w as f32 / 2.0).min(h as f32 / 2.0).max(0.0);
                box_sdf(px, py, x as f32 + w as f32 / 2.0, y as f32 + h as f32 / 2.0, w as f32 / 2.0, h as f32 / 2.0, r)
            }
            Shape::Notch { x, w, h, r } => {
                let (x, w, h) = (x as f32, w as f32, h as f32);
                let r = (r as f32).min(w / 2.0).min(h).max(0.0);
                // the body reaches above the screen so its top corners and border never show
                let ext = r + border + 4.0;
                let body = box_sdf(px, py, x + w / 2.0, (h - ext) / 2.0, w / 2.0, (h + ext) / 2.0, r);
                // a concave fillet on each side: the square next to the body minus a disc
                let ov = border + 3.0; // the fillet reaches into the body so there is no seam (and no line) where they meet
                let fillet = |sq_cx: f32, disc_cx: f32| {
                    let sq = box_sdf(px, py, sq_cx, (r - ext) / 2.0, (r + ov) / 2.0, (r + ext) / 2.0, 0.0);
                    let disc = ((px - disc_cx).powi(2) + (py - r).powi(2)).sqrt() - r;
                    sq.max(-disc)
                };
                body.min(fillet(x - r / 2.0 + ov / 2.0, x - r)).min(fillet(x + w + r / 2.0 - ov / 2.0, x + w + r))
            }
        }
    }

    /// Pixel bounds (x0, y0, x1, y1) that can be touched, with a little margin.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        match *self {
            Shape::Rect { x, y, w, h } | Shape::Round { x, y, w, h, .. } => (x - 2, y - 2, x + w + 2, y + h + 2),
            Shape::Notch { x, w, h, r } => (x - r - 2, 0, x + w + r + 2, h + 2),
        }
    }

    /// The part to treat as the bar's body when something else (blur) needs a plain rectangle: [x, y, w, h].
    pub fn body(&self) -> [i32; 4] {
        match *self {
            Shape::Rect { x, y, w, h } | Shape::Round { x, y, w, h, .. } => [x, y, w, h],
            Shape::Notch { x, w, h, .. } => [x, 0, w, h],
        }
    }
}

fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Fill `shape` with `fill` and draw a `border`-wide outline just inside its edge.
pub fn paint(c: &mut Canvas, shape: &Shape, fill: Rgba, border_w: i32, border: Rgba) {
    let bw = border_w.max(0) as f32;
    let (x0, y0, x1, y1) = shape.bounds();
    for y in y0.max(0)..y1.min(c.h) {
        for x in x0.max(0)..x1.min(c.w) {
            let d = shape.sdf(x as f32 + 0.5, y as f32 + 0.5, bw);
            let outer = cover(d);
            if outer <= 0.0 {
                continue;
            }
            let bd = shape.border_sdf(x as f32 + 0.5, y as f32 + 0.5, bw);
            let mask = shape.border_mask(y as f32 + 0.5);
            let ring = if bw > 0.0 { (cover(bd) - cover(bd + bw)).max(0.0) * mask } else { 0.0 };
            let ring = ring.min(outer);
            let inner = outer - ring;
            if inner > 0.0 {
                c.blend(x, y, fill, (inner * fill[3] as f32) as u8);
            }
            if ring > 0.0 {
                c.blend(x, y, border, (ring * border[3] as f32) as u8);
            }
        }
    }
}

/// Like [`paint`], but the fill and border colours' own alpha is applied once (as the notification cards do): `paint` also
/// scales it by the coverage's alpha, so a colour at 85 % ended up about 72 % opaque.
pub fn paint_exact(c: &mut Canvas, shape: &Shape, fill: Rgba, border_w: i32, border: Rgba) {
    let bw = border_w.max(0) as f32;
    let (x0, y0, x1, y1) = shape.bounds();
    for y in y0.max(0)..y1.min(c.h) {
        for x in x0.max(0)..x1.min(c.w) {
            let outer = cover(shape.sdf(x as f32 + 0.5, y as f32 + 0.5, bw));
            if outer <= 0.0 {
                continue;
            }
            let bd = shape.border_sdf(x as f32 + 0.5, y as f32 + 0.5, bw);
            let mask = shape.border_mask(y as f32 + 0.5);
            let ring = if bw > 0.0 { (cover(bd) - cover(bd + bw)).max(0.0) * mask } else { 0.0 };
            let ring = ring.min(outer);
            let inner = outer - ring;
            if inner > 0.0 {
                c.blend(x, y, fill, (inner * 255.0) as u8);
            }
            if ring > 0.0 {
                c.blend(x, y, border, (ring * 255.0) as u8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_and_round_inside_outside() {
        let r = Shape::Rect { x: 10, y: 10, w: 20, h: 10 };
        assert!(r.sdf(20.0, 15.0, 0.0) < 0.0);
        assert!(r.sdf(5.0, 15.0, 0.0) > 0.0);
        let rr = Shape::Round { x: 0, y: 0, w: 40, h: 20, r: 10 };
        assert!(rr.sdf(0.5, 0.5, 0.0) > 0.0, "the corner is cut off");
        assert!(rr.sdf(20.0, 10.0, 0.0) < 0.0);
    }

    #[test]
    fn notch_hangs_from_the_top_and_flares_out() {
        let n = Shape::Notch { x: 100, w: 200, h: 30, r: 10 };
        assert!(n.sdf(200.0, 1.0, 1.0) < 0.0, "touches the top edge");
        assert!(n.sdf(200.0, 29.0, 1.0) < 0.0);
        assert!(n.sdf(105.0, 29.8, 1.0) > -0.2, "bottom corner is rounded away");
        // just outside the body at the top there is a fillet; far from it there is nothing
        assert!(n.sdf(95.0, 1.0, 1.0) < 0.5);
        assert!(n.sdf(60.0, 1.0, 1.0) > 0.0);
        assert!(n.sdf(95.0, 20.0, 1.0) > 0.0, "below the fillet is outside");
    }

    #[test]
    fn paint_draws_border_ring_and_fill() {
        let (w, h) = (60, 30);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let mut c = Canvas::new(&mut buf, w as u32, h as u32);
        paint(&mut c, &Shape::Round { x: 5, y: 5, w: 50, h: 20, r: 6 }, [0, 0, 255, 255], 2, [255, 0, 0, 255]);
        let px = |x: i32, y: i32| {
            let i = ((y * w + x) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert_eq!(px(30, 15), [255, 0, 0, 255], "fill (BGRA premultiplied blue)");
        assert!(px(30, 5)[2] > 200, "border row is red");
        assert_eq!(px(1, 1), [0, 0, 0, 0], "outside is untouched");
    }
}

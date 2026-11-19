//! The login screen's picture: the desktop wallpaper, blurred and darkened.

use image::{imageops::FilterType, RgbaImage};

/// `w` x `h` BGRA pixels of `path` (covering the whole screen), blurred by `blur` (0..=100) and darkened by `dim` percent.
/// `None` when the image cannot be read, so the caller falls back to its gradient.
pub fn render(path: &str, w: u32, h: u32, blur: u32, dim: u32) -> Option<Vec<u8>> {
    let img = image::open(path).ok()?.to_rgba8();
    // Work on a small copy: blurring a quarter-size picture is 16 times cheaper and looks the same once scaled back up.
    let (sw, sh) = ((w / 4).max(1), (h / 4).max(1));
    let mut small = cover(&img, sw, sh);
    let radius = (blur.min(100) as f32 / 100.0 * sw as f32 / 12.0).round() as usize;
    if radius > 0 {
        for _ in 0..3 {
            small = box_blur(&small, radius);
        }
    }
    let big = if (sw, sh) == (w, h) { small } else { image::imageops::resize(&small, w, h, FilterType::Triangle) };
    let keep = 1.0 - dim.min(90) as f32 / 100.0;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for p in big.pixels() {
        out.extend_from_slice(&[(p[2] as f32 * keep) as u8, (p[1] as f32 * keep) as u8, (p[0] as f32 * keep) as u8, 0xff]);
    }
    Some(out)
}

/// Scale `img` to cover `w` x `h` and crop the overflow from the middle.
fn cover(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let s = (w as f32 / img.width() as f32).max(h as f32 / img.height() as f32);
    let (nw, nh) = (((img.width() as f32 * s).round() as u32).max(w), ((img.height() as f32 * s).round() as u32).max(h));
    let scaled = image::imageops::resize(img, nw, nh, FilterType::Triangle);
    image::imageops::crop_imm(&scaled, (nw - w) / 2, (nh - h) / 2, w, h).to_image()
}

/// One box-blur pass (horizontal, then vertical) with edge clamping.
fn box_blur(img: &RgbaImage, r: usize) -> RgbaImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let src = img.as_raw();
    let pass = |src: &[u8], dst: &mut [u8], len: usize, lines: usize, step: usize, line_step: usize| {
        let n = (2 * r + 1) as u32;
        for line in 0..lines {
            let base = line * line_step;
            for c in 0..4 {
                let at = |i: isize| src[base + (i.clamp(0, len as isize - 1) as usize) * step + c] as u32;
                let mut sum: u32 = (-(r as isize)..=r as isize).map(at).sum();
                for i in 0..len {
                    dst[base + i * step + c] = (sum / n) as u8;
                    sum = sum + at(i as isize + r as isize + 1) - at(i as isize - r as isize);
                }
            }
        }
    };
    let mut tmp = vec![0u8; src.len()];
    pass(src, &mut tmp, w, h, 4, w * 4);
    let mut out = vec![0u8; src.len()];
    pass(&tmp, &mut out, h, w, w * 4, 4);
    RgbaImage::from_raw(w as u32, h as u32, out).expect("same size")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_a_flat_picture_flat_and_spreads_an_edge() {
        let flat = RgbaImage::from_pixel(20, 10, image::Rgba([100, 100, 100, 255]));
        assert!(box_blur(&flat, 3).pixels().all(|p| p[0] == 100));
        let mut edge = RgbaImage::from_pixel(20, 10, image::Rgba([0, 0, 0, 255]));
        for y in 0..10 {
            for x in 10..20 {
                edge.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let b = box_blur(&edge, 3);
        let v = b.get_pixel(10, 5)[0];
        assert!(v > 60 && v < 200, "{v}");
    }
}

#[cfg(test)]
mod preview {
    #[test]
    #[ignore]
    fn writes_a_preview() {
        let px = super::render(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/wallpapers/singularity.jpg"), 960, 540, 40, 35).unwrap();
        let rgba: Vec<u8> = px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        image::RgbaImage::from_raw(960, 540, rgba).unwrap().save("/tmp/claude-1001/greeter-bg.png").unwrap();
    }
}

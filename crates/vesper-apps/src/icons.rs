//! Icon lookup (freedesktop icon themes, simplified) and rasterisation to a fixed size.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use resvg::{tiny_skia, usvg};
use vesper_gfx::Image;

use crate::desktop::data_dirs;

#[derive(Default)]
pub struct Icons {
    cache: HashMap<(String, i32), Option<Image>>,
}

impl Icons {
    /// An icon `size`x`size` pixels, or None if no file was found or it couldn't be decoded.
    pub fn get(&mut self, name: &str, size: i32) -> Option<Image> {
        self.cache.entry((name.to_owned(), size)).or_insert_with(|| load(name, size)).clone()
    }
}

fn load(name: &str, size: i32) -> Option<Image> {
    if name.is_empty() {
        return None;
    }
    let path = if name.starts_with('/') { PathBuf::from(name) } else { find(name, size as u32)? };
    let bytes = fs::read(&path).ok()?;
    let mut target = tiny_skia::Pixmap::new(size as u32, size as u32)?;
    let is_svg = path.extension().is_some_and(|e| e == "svg" || e == "svgz");
    if is_svg {
        let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default()).ok()?;
        let s = tree.size();
        let scale = size as f32 / s.width().max(s.height());
        let (ox, oy) = ((size as f32 - s.width() * scale) / 2.0, (size as f32 - s.height() * scale) / 2.0);
        let ts = tiny_skia::Transform::from_scale(scale, scale).post_translate(ox, oy);
        resvg::render(&tree, ts, &mut target.as_mut());
    } else {
        let src = tiny_skia::Pixmap::decode_png(&bytes).ok()?;
        let scale = size as f32 / src.width().max(src.height()) as f32;
        let (ox, oy) = (
            (size as f32 - src.width() as f32 * scale) / 2.0,
            (size as f32 - src.height() as f32 * scale) / 2.0,
        );
        let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() };
        let ts = tiny_skia::Transform::from_scale(scale, scale).post_translate(ox, oy);
        target.draw_pixmap(0, 0, src.as_ref(), &paint, ts, None);
    }
    Some(Image::from_rgba(size, size, target.data(), true))
}

/// Pick the best file for `name`: scalable SVG if there is one, else the smallest PNG that is at least
/// `size`, else the largest PNG.
fn find(name: &str, size: u32) -> Option<PathBuf> {
    let mut svg: Option<PathBuf> = None;
    let mut png: Vec<(u32, PathBuf)> = Vec::new();

    let mut roots: Vec<PathBuf> = data_dirs().into_iter().map(|d| d.join("icons")).collect();
    if let Some(h) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(h).join(".icons"));
    }
    for root in &roots {
        let Ok(themes) = fs::read_dir(root) else { continue };
        let mut themes: Vec<PathBuf> = themes.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        // hicolor is the spec's fallback theme; try it first so results are predictable.
        themes.sort_by_key(|p| (p.file_name().is_none_or(|n| n != "hicolor"), p.clone()));
        for theme in themes {
            let Ok(sizes) = fs::read_dir(&theme) else { continue };
            for dir in sizes.flatten().map(|e| e.path()) {
                let Some(dname) = dir.file_name().and_then(|n| n.to_str()) else { continue };
                let apps = dir.join("apps");
                if dname == "scalable" {
                    let f = apps.join(format!("{name}.svg"));
                    if svg.is_none() && f.is_file() {
                        svg = Some(f);
                    }
                } else if let Some(px) = dname.split('x').next().and_then(|s| s.parse::<u32>().ok()) {
                    let f = apps.join(format!("{name}.png"));
                    if f.is_file() {
                        png.push((px, f));
                    }
                }
            }
        }
    }
    if let Some(s) = svg {
        return Some(s);
    }
    png.sort();
    if let Some((_, p)) = png.iter().find(|(px, _)| *px >= size) {
        return Some(p.clone());
    }
    if let Some((_, p)) = png.last() {
        return Some(p.clone());
    }
    for dir in data_dirs().into_iter().map(|d| d.join("pixmaps")).chain([Path::new("/usr/share/pixmaps").to_owned()]) {
        for ext in ["svg", "png"] {
            let f = dir.join(format!("{name}.{ext}"));
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

/// Render SVG markup at exactly `w` x `h` pixels (smooth edges, no scaling blur), or None if it does not parse.
pub fn render_svg_data(svg: &str, w: u32, h: u32) -> Option<Image> {
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).ok()?;
    let mut target = tiny_skia::Pixmap::new(w, h)?;
    let s = tree.size();
    let ts = tiny_skia::Transform::from_scale(w as f32 / s.width(), h as f32 / s.height());
    resvg::render(&tree, ts, &mut target.as_mut());
    Some(Image::from_rgba(w as i32, h as i32, target.data(), true))
}

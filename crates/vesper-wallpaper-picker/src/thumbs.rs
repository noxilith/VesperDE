//! Thumbnails for the wallpaper grid: decoded on worker threads and cached on disk, so reopening is instant.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use image::{imageops::FilterType, RgbaImage};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

pub const THUMB_W: u32 = 640;
pub const THUMB_H: u32 = 360;

/// The colour families the changer can filter by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTag {
    Red,
    Orange,
    Green,
    Blue,
    Gray,
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (if h < 0.0 { h + 360.0 } else { h }, if max == 0.0 { 0.0 } else { d / max }, max)
}

/// The dominant colour family of an image: the strongest hue among its colourful pixels, or gray if it has few.
pub fn classify(img: &RgbaImage) -> ColorTag {
    let (mut bins, mut colourful, mut total) = ([0f32; 4], 0u32, 0u32);
    for p in img.pixels().step_by(5) {
        let (h, s, v) = rgb_to_hsv(p.0[0], p.0[1], p.0[2]);
        total += 1;
        if s > 0.28 && v > 0.22 {
            colourful += 1;
            let bin = match h {
                h if !(20.0..330.0).contains(&h) => 0, // red
                h if h < 70.0 => 1,                     // orange and yellow
                h if h < 170.0 => 2,                    // green
                h if h < 290.0 => 3,                    // cyan, blue, violet
                _ => 0,                                 // pink and magenta count as red
            };
            bins[bin] += s * v;
        }
    }
    if total == 0 || (colourful as f32) < total as f32 * 0.18 {
        return ColorTag::Gray;
    }
    let best = (0..4).max_by(|&a, &b| bins[a].total_cmp(&bins[b])).unwrap_or(0);
    [ColorTag::Red, ColorTag::Orange, ColorTag::Green, ColorTag::Blue][best]
}

/// Image files in `dir` (png, jpeg, webp), sorted by name.
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"))
        })
        .collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    files
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("vesper/wallpaper-thumbs"))
}

/// FNV-1a over the path, size and modification time: a changed file gets a new thumbnail.
pub fn cache_key(path: &Path, len: u64, mtime_secs: u64) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.to_string_lossy().bytes().chain(len.to_le_bytes()).chain(mtime_secs.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Scale `img` to cover THUMB_W x THUMB_H and crop the overflow from the centre.
fn cover(img: &RgbaImage) -> RgbaImage {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let s = (THUMB_W as f32 / iw).max(THUMB_H as f32 / ih);
    let (nw, nh) = ((iw * s).ceil().max(THUMB_W as f32) as u32, (ih * s).ceil().max(THUMB_H as f32) as u32);
    let resized = image::imageops::resize(img, nw, nh, FilterType::Triangle);
    image::imageops::crop_imm(&resized, (nw - THUMB_W) / 2, (nh - THUMB_H) / 2, THUMB_W, THUMB_H).to_image()
}

fn load(path: &Path) -> Option<RgbaImage> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let cached = cache_dir().map(|d| d.join(format!("{}.png", cache_key(path, meta.len(), mtime))));
    if let Some(file) = &cached {
        if let Ok(img) = image::open(file) {
            let img = img.to_rgba8();
            if img.width() == THUMB_W && img.height() == THUMB_H {
                return Some(img);
            }
        }
    }
    let thumb = cover(&image::open(path).ok()?.to_rgba8());
    if let Some(file) = cached {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = thumb.save(&file);
    }
    Some(thumb)
}

/// Start `threads` workers that load thumbnails for `paths` and send `(index, rgba bytes)` as each is ready.
pub fn spawn_loaders(paths: Vec<PathBuf>, threads: usize, tx: Sender<(usize, Vec<u8>, ColorTag)>) {
    let queue: Arc<Mutex<VecDeque<usize>>> = Arc::new(Mutex::new((0..paths.len()).collect()));
    let paths = Arc::new(paths);
    for _ in 0..threads.max(1) {
        let (queue, paths, tx) = (queue.clone(), paths.clone(), tx.clone());
        std::thread::spawn(move || loop {
            let Some(i) = queue.lock().unwrap().pop_front() else { return };
            if let Some(img) = load(&paths[i]) {
                let tag = classify(&img);
                if tx.send((i, img.into_raw(), tag)).is_err() {
                    return;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_the_file() {
        let a = cache_key(Path::new("/w/a.jpg"), 100, 5);
        assert_eq!(a, cache_key(Path::new("/w/a.jpg"), 100, 5));
        assert_ne!(a, cache_key(Path::new("/w/a.jpg"), 101, 5));
        assert_ne!(a, cache_key(Path::new("/w/a.jpg"), 100, 6));
        assert_ne!(a, cache_key(Path::new("/w/b.jpg"), 100, 5));
    }

    #[test]
    fn cover_makes_exact_thumbnail_size() {
        let wide = RgbaImage::new(3840, 1000);
        let tall = RgbaImage::new(500, 2000);
        for img in [wide, tall] {
            let t = cover(&img);
            assert_eq!((t.width(), t.height()), (THUMB_W, THUMB_H));
        }
    }

    fn solid(r: u8, g: u8, b: u8) -> RgbaImage {
        RgbaImage::from_pixel(40, 40, image::Rgba([r, g, b, 255]))
    }

    #[test]
    fn classifies_colours() {
        assert_eq!(classify(&solid(220, 40, 50)), ColorTag::Red);
        assert_eq!(classify(&solid(245, 150, 40)), ColorTag::Orange);
        assert_eq!(classify(&solid(60, 190, 80)), ColorTag::Green);
        assert_eq!(classify(&solid(50, 100, 230)), ColorTag::Blue);
        assert_eq!(classify(&solid(128, 130, 135)), ColorTag::Gray);
        assert_eq!(classify(&solid(250, 250, 250)), ColorTag::Gray);
        assert_eq!(classify(&solid(10, 10, 12)), ColorTag::Gray);
    }

    #[test]
    fn scan_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("vesper-wp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["b.PNG", "a.jpg", "notes.txt", "c.webp"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        let names: Vec<String> = scan(&dir).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.jpg", "b.PNG", "c.webp"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

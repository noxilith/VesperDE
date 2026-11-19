//! A built-in arrow cursor, so the pointer is visible on a bare tty even with no cursor theme.

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

const ARROW: [&str; 19] = [
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.....XXXXX ",
    "X..X..X     ",
    "X.X X..X    ",
    "XX  X..X    ",
    "X    X..X   ",
    "     X..X   ",
    "      X..X  ",
    "      X..X  ",
    "       XX   ",
];

pub fn arrow() -> MemoryRenderBuffer {
    let (w, h) = (ARROW[0].len(), ARROW.len());
    let mut px = Vec::with_capacity(w * h * 4);
    for row in ARROW {
        for c in row.chars() {
            // Argb8888 little-endian byte order: B, G, R, A
            px.extend_from_slice(match c {
                'X' => &[0x10, 0x10, 0x10, 0xff],
                '.' => &[0xf5, 0xf5, 0xf5, 0xff],
                _ => &[0, 0, 0, 0],
            });
        }
    }
    MemoryRenderBuffer::from_slice(&px, Fourcc::Argb8888, (w as i32, h as i32), 1, Transform::Normal, None)
}

use std::collections::HashMap;

use smithay::input::pointer::CursorIcon;

/// One cursor image: the buffer and where its tip is, in logical pixels.
struct Shape {
    buf: MemoryRenderBuffer,
    hot: (i32, i32),
}

/// The pointer images for a cursor theme and size, loaded up front (a few dozen small pictures), with the built-in arrow
/// as the fallback when the theme is missing or lacks a shape.
pub struct CursorSet {
    theme: String,
    size: u32,
    scale: u32,
    arrow: MemoryRenderBuffer,
    shapes: HashMap<&'static str, Shape>,
    /// A themed set that came out empty (files not there yet at session start?) is tried again a few times.
    tries: u8,
    loaded_at: std::time::Instant,
}

/// The shapes apps ask for (CSS cursor names; the theme's own aliases cover the old X names).
const NAMES: &[&str] = &[
    "default", "context-menu", "help", "pointer", "progress", "wait", "cell", "crosshair", "text", "vertical-text", "alias",
    "copy", "move", "no-drop", "not-allowed", "grab", "grabbing", "all-scroll", "col-resize", "row-resize", "n-resize",
    "e-resize", "s-resize", "w-resize", "ne-resize", "nw-resize", "se-resize", "sw-resize", "ew-resize", "ns-resize",
    "nesw-resize", "nwse-resize", "zoom-in", "zoom-out",
];

impl CursorSet {
    /// Load `theme` (empty = only the built-in arrow) at `size` logical pixels, for an output scale of `scale`.
    pub fn load(theme: &str, size: u32, scale: u32) -> Self {
        let size = size.clamp(12, 128);
        let scale = scale.max(1);
        let mut set = CursorSet { theme: theme.to_owned(), size, scale, arrow: arrow(), shapes: HashMap::new(), tries: 0, loaded_at: std::time::Instant::now() };
        if theme.is_empty() {
            return set;
        }
        let xt = xcursor::CursorTheme::load(theme);
        for &name in NAMES {
            // "default" is called left_ptr in most themes (the theme's alias files usually cover it, but be sure)
            let path = xt.load_icon(name).or_else(|| (name == "default").then(|| xt.load_icon("left_ptr")).flatten());
            let Some(images) = path.and_then(|p| std::fs::read(p).ok()).and_then(|d| xcursor::parser::parse_xcursor(&d)) else { continue };
            // the frame closest to the size we want (animated cursors show their first frame)
            let want = size * scale;
            let Some(best) = images.iter().min_by_key(|i| (i.size as i64 - want as i64).abs()).map(|i| i.size) else { continue };
            let Some(img) = images.iter().find(|i| i.size == best) else { continue };
            // xcursor gives RGBA (premultiplied); the buffer wants B, G, R, A
            let px: Vec<u8> = img.pixels_rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            // an image drawn at 48 px for a 24 px cursor on a 1x screen is shown at half size
            let ratio = (img.size as f32 / size as f32).max(1.0).round() as i32;
            let buf = MemoryRenderBuffer::from_slice(&px, Fourcc::Argb8888, (img.width as i32, img.height as i32), ratio, Transform::Normal, None);
            set.shapes.insert(name, Shape { buf, hot: (img.xhot as i32 / ratio, img.yhot as i32 / ratio) });
        }
        if set.shapes.is_empty() {
            tracing::warn!("cursor theme {theme:?} not found or empty: using the built-in arrow");
        } else {
            tracing::info!("cursor theme {theme:?}, size {size}: {} shapes", set.shapes.len());
        }
        set
    }

    /// Load again after `prev` came out empty, remembering how often we tried.
    pub fn reload(prev: &CursorSet, theme: &str, size: u32, scale: u32) -> Self {
        let mut set = Self::load(theme, size, scale);
        if prev.theme == theme {
            set.tries = prev.tries + 1;
        }
        set
    }

    /// Is this the set for these settings?
    pub fn is_for(&self, theme: &str, size: u32, scale: u32) -> bool {
        let stale = !self.theme.is_empty() && self.shapes.is_empty() && self.tries < 5 && self.loaded_at.elapsed() > std::time::Duration::from_secs(3);
        !stale && self.theme == theme && self.size == size.clamp(12, 128) && self.scale == scale.max(1)
    }

    /// The image for a pointer shape and its hotspot in logical pixels (the built-in arrow when there is none).
    pub fn get(&self, icon: CursorIcon) -> (&MemoryRenderBuffer, (i32, i32)) {
        let name = icon.name();
        let pick = self.shapes.get(name).or_else(|| icon.alt_names().iter().find_map(|a| self.shapes.get(a))).or_else(|| self.shapes.get("default"));
        match pick {
            Some(s) => (&s.buf, s.hot),
            None => (&self.arrow, (0, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> String {
        format!("{}/../../assets/cursors", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn bundled_themes_load_with_their_shapes() {
        std::env::set_var("XCURSOR_PATH", bundled());
        for theme in ["Bibata-Modern-Classic", "Bibata-Modern-Ice", "Bibata-Modern-Amber", "phinger-cursors-dark", "phinger-cursors-light"] {
            let set = CursorSet::load(theme, 24, 1);
            assert!(set.shapes.len() >= 20, "{theme}: only {} shapes", set.shapes.len());
            assert!(set.shapes.contains_key("default") && set.shapes.contains_key("text") && set.shapes.contains_key("pointer"), "{theme}");
            // the arrow's tip is inside the image
            let (_, hot) = set.get(CursorIcon::Default);
            assert!(hot.0 >= 0 && hot.1 >= 0 && hot.0 < 48 && hot.1 < 48, "{theme}: hotspot {hot:?}");
        }
    }

    #[test]
    fn missing_theme_falls_back_to_the_arrow() {
        std::env::set_var("XCURSOR_PATH", bundled());
        let set = CursorSet::load("No-Such-Theme", 24, 1);
        assert!(set.shapes.is_empty());
        assert_eq!(set.get(CursorIcon::Text).1, (0, 0));
    }

    #[test]
    fn set_knows_its_settings() {
        let set = CursorSet::load("", 24, 1);
        assert!(set.is_for("", 24, 1) && !set.is_for("x", 24, 1) && !set.is_for("", 32, 1) && !set.is_for("", 24, 2));
    }
}

/// Make the cursor theme visible to apps that do not use our pointer shapes (older toolkits, Electron, Xwayland): the theme is
/// linked into `~/.local/share/icons`, `~/.icons/default` inherits it, and GTK's settings name it. Files that were not
/// written by Vesper are left alone.
pub fn install_for_apps(theme: &str, size: u32) {
    use std::{fs, path::PathBuf};
    const MARK: &str = "# Written by Vesper: the cursor theme in Settings > Appearance > Cursor";
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    if theme.is_empty() {
        return;
    }
    // 1. the theme where every app looks (only if it lives somewhere they do not)
    let standard = [home.join(".local/share/icons"), home.join(".icons"), PathBuf::from("/usr/share/icons"), PathBuf::from("/usr/local/share/icons")];
    if let Some(src) = vesper_config::cursor_dirs().into_iter().map(|d| d.join(theme)).find(|d| d.join("cursors").is_dir()) {
        let in_standard = standard.iter().any(|s| src.parent().is_some_and(|p| p.canonicalize().ok() == s.canonicalize().ok()));
        if !in_standard {
            let link = home.join(".local/share/icons").join(theme);
            let _ = fs::create_dir_all(home.join(".local/share/icons"));
            let ours = fs::read_link(&link).is_ok();
            if ours || !link.exists() {
                let _ = fs::remove_file(&link);
                let _ = std::os::unix::fs::symlink(&src, &link);
            }
        }
    }
    // 2. the "default" theme that apps fall back to
    let default_theme = home.join(".icons/default/index.theme");
    let existing = fs::read_to_string(&default_theme).ok();
    if existing.as_deref().is_none_or(|t| t.contains(MARK)) {
        let _ = fs::create_dir_all(home.join(".icons/default"));
        let _ = fs::write(&default_theme, format!("{MARK}\n[Icon Theme]\nName=Default\nComment=Default cursor theme\nInherits={theme}\n"));
    }
    // 3. GTK 3 and 4
    for dir in ["gtk-3.0", "gtk-4.0"] {
        let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let path = base.join(dir).join("settings.ini");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let updated = set_ini(&text, "Settings", &[("gtk-cursor-theme-name", theme), ("gtk-cursor-theme-size", &size.to_string())]);
        if updated != text {
            let _ = fs::create_dir_all(path.parent().unwrap_or(&base));
            let _ = fs::write(&path, updated);
        }
    }
}

/// Set keys in a section of an ini file, keeping everything else as it is.
fn set_ini(text: &str, section: &str, keys: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let header = format!("[{section}]");
    let start = match lines.iter().position(|l| l.trim() == header) {
        Some(i) => i,
        None => {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.len() - 1
        }
    };
    for (k, v) in keys {
        let line = format!("{k}={v}");
        // where the section ends changes as keys are added, so look again for each key
        let end = lines.iter().enumerate().skip(start + 1).find(|(_, l)| l.trim_start().starts_with('[')).map_or(lines.len(), |(i, _)| i);
        match lines[start + 1..end].iter().position(|l| l.split('=').next().is_some_and(|name| name.trim() == *k)) {
            Some(i) => lines[start + 1 + i] = line,
            None => {
                // before any blank lines that end the section
                let mut at = end;
                while at > start + 1 && lines[at - 1].is_empty() {
                    at -= 1;
                }
                lines.insert(at, line);
            }
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn ini_keys_are_set_without_touching_the_rest() {
        let out = set_ini("[Settings]\ngtk-theme-name=Adwaita\ngtk-cursor-theme-name=Old\n\n[Other]\nx=1\n", "Settings", &[("gtk-cursor-theme-name", "Bibata"), ("gtk-cursor-theme-size", "24")]);
        assert!(out.contains("gtk-theme-name=Adwaita") && out.contains("gtk-cursor-theme-name=Bibata") && out.contains("gtk-cursor-theme-size=24"));
        assert!(!out.contains("Old") && out.contains("[Other]\nx=1"));
        let fresh = set_ini("", "Settings", &[("a", "1")]);
        assert_eq!(fresh, "[Settings]\na=1\n");
        // setting it again changes nothing
        assert_eq!(set_ini(&fresh, "Settings", &[("a", "1")]), fresh);
    }
}

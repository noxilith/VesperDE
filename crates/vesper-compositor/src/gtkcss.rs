//! Titlebars that clients draw themselves. Vesper asks every client for server-side decoration (and draws none), which
//! most toolkits honour. GTK 3 does not: Tauri apps (Astralis, flatpak or not) get GTK's own fallback titlebar. That one is
//! hidden through a block in `~/.config/gtk-3.0/gtk.css`, which only matches GTK's *default* titlebar (`default-decoration`),
//! never a header bar an app builds itself with its own buttons.
//! A flatpak app does not read `~/.config`, so the same block goes into each `~/.var/app/<id>/config/gtk-3.0/gtk.css`.

use std::{fs, path::PathBuf};

const BEGIN: &str = "/* vesper: hide default titlebars (Settings > Appearance) */";
const END: &str = "/* vesper: end */";
const CSS: &str = "headerbar.default-decoration, .titlebar.default-decoration {
    min-height: 0; padding: 0; margin: 0; border: none; box-shadow: none; background: none; font-size: 1px;
}
.default-decoration button.titlebutton { min-height: 0; min-width: 0; padding: 0; margin: 0; border: none; }
headerbar.default-decoration label { min-height: 0; margin: 0; padding: 0; }";

/// The text of `css` with our block set (`on`) or removed. Everything outside the block stays as it is.
fn with_block(css: &str, on: bool) -> String {
    let (head, tail) = match (css.find(BEGIN), css.find(END)) {
        (Some(a), Some(b)) if b > a => (&css[..a], &css[b + END.len()..]),
        _ => (css, ""),
    };
    let rest = format!("{}{}", head.trim_end(), tail).trim().to_string();
    let rest = if rest.is_empty() { String::new() } else { format!("{rest}\n\n") };
    if on { format!("{rest}{BEGIN}\n{CSS}\n{END}\n") } else if rest.is_empty() { String::new() } else { rest.trim_end().to_string() + "\n" }
}

/// Hide (or stop hiding) GTK 3's own titlebars for the apps of this user.
pub fn apply(hide: bool) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    apply_in(&base, hide, true);
    // A flatpak app reads its own config folder (`~/.var/app/<id>/config`), not `~/.config`: give each one the same block.
    if let Ok(apps) = fs::read_dir(home.join(".var/app")) {
        for app in apps.flatten() {
            apply_in(&app.path().join("config"), hide, false);
        }
    }
}

/// The block in `<config>/gtk-3.0/gtk.css`. `create_config`: make `config` itself if it is missing (otherwise it is skipped).
fn apply_in(config: &std::path::Path, hide: bool, create_config: bool) {
    if !create_config && !config.is_dir() {
        return;
    }
    let dir = config.join("gtk-3.0");
    let css = dir.join("gtk.css");
    let text = fs::read_to_string(&css).unwrap_or_default();
    write_or_remove(&css, &text, with_block(&text, hide), config);
    let ini = dir.join("settings.ini");
    let text = fs::read_to_string(&ini).unwrap_or_default();
    write_or_remove(&ini, &text, ini_with_layout(&text, hide), config);
}

/// `settings.ini` with an empty `gtk-decoration-layout` (no window buttons: the titlebar then has nothing in it to keep it
/// tall, and the CSS above shrinks it to nothing) or without it. Off only removes an *empty* layout, which is ours.
fn ini_with_layout(text: &str, on: bool) -> String {
    let is_layout = |l: &str| l.trim().starts_with("gtk-decoration-layout");
    let ours = |l: &str| l.split_once('=').is_some_and(|(_, v)| v.trim().is_empty());
    let mut lines: Vec<String> = text.lines().filter(|l| !(is_layout(l) && (on || ours(l)))).map(str::to_owned).collect();
    if on {
        match lines.iter().position(|l| l.trim() == "[Settings]") {
            Some(i) => lines.insert(i + 1, "gtk-decoration-layout=".into()),
            None => {
                if lines.last().is_some_and(|l| !l.is_empty()) {
                    lines.push(String::new());
                }
                lines.push("[Settings]".into());
                lines.push("gtk-decoration-layout=".into());
            }
        }
    }
    if lines.iter().all(|l| l.trim().is_empty() || l.trim() == "[Settings]") {
        return String::new(); // nothing of the user's left in it
    }
    let out = lines.join("\n");
    out + "\n"
}

fn write_or_remove(path: &std::path::Path, text: &str, updated: String, config: &std::path::Path) {
    if updated == text {
        return;
    }
    if updated.is_empty() {
        let _ = fs::remove_file(path);
    } else {
        let _ = fs::create_dir_all(path.parent().unwrap_or(config));
        let _ = fs::write(path, updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_decoration_layout_is_added_and_removed() {
        let on = ini_with_layout("[Settings]\ngtk-cursor-theme-name=Bibata\n", true);
        assert!(on.contains("gtk-decoration-layout=\n") && on.contains("gtk-cursor-theme-name=Bibata"));
        assert_eq!(ini_with_layout(&on, true), on, "applying twice changes nothing");
        assert_eq!(ini_with_layout(&on, false), "[Settings]\ngtk-cursor-theme-name=Bibata\n");
        assert_eq!(ini_with_layout("", true), "[Settings]\ngtk-decoration-layout=\n");
        assert_eq!(ini_with_layout(&ini_with_layout("", true), false), "");
        // a layout the user chose stays when we switch off
        assert_eq!(ini_with_layout("[Settings]\ngtk-decoration-layout=menu:close\n", false), "[Settings]\ngtk-decoration-layout=menu:close\n");
    }

    #[test]
    fn block_is_added_replaced_and_removed() {
        let mine = "button { color: red; }\n";
        let on = with_block(mine, true);
        assert!(on.starts_with(mine.trim_end()) && on.contains(BEGIN) && on.contains(END));
        assert_eq!(with_block(&on, true), on, "applying twice changes nothing");
        assert_eq!(with_block(&on, false), mine);
        assert_eq!(with_block("", false), "");
        assert_eq!(with_block(&with_block("", true), false), "");
    }
}

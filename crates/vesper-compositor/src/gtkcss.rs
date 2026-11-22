//! How GTK 3 apps look in a Vesper session, set through the user's `gtk.css` and `settings.ini`:
//!
//! * **Colours** (`theme.gtk`): the wallpaper palette, as named colours that GTK's own theme is built from. Dialogs such as
//!   the keyring's password prompt (`gcr-prompter`) then match the File Manager and follow the wallpaper. GTK reads the file
//!   when an app starts, so open apps keep their colours until restarted.
//! * **No titlebar** (`appearance.hide_client_titlebars`): Vesper asks every client for server-side decoration (and draws
//!   none), which most toolkits honour. GTK 3 does not: Tauri apps (Astralis) get GTK's own fallback titlebar. CSS shrinks
//!   that one (`default-decoration` only, never a header bar an app builds itself), and a `gtk-decoration-layout` with no buttons (`:`)
//!   takes its buttons away so nothing keeps it tall.
//!
//! Both live in one block of `~/.config/gtk-3.0/gtk.css` between markers. A flatpak app reads its own config folder
//! (`~/.var/app/<id>/config`), not `~/.config`, so each of those gets the same files.

use std::{fs, path::PathBuf, sync::Mutex, time::SystemTime};

use vesper_config::theme::{Palette, ThemeSettings};

const BEGIN: &str = "/* vesper: GTK 3 look (Settings > Appearance and Colours) */";
/// What the block was called when it only hid titlebars: still recognised, so it is replaced and not duplicated.
const OLD_BEGIN: &str = "/* vesper: hide default titlebars (Settings > Appearance) */";
const END: &str = "/* vesper: end */";

const TITLEBAR_CSS: &str = "headerbar.default-decoration, .titlebar.default-decoration {
    min-height: 0; padding: 0; margin: 0; border: none; box-shadow: none; background: none; font-size: 1px;
}
.default-decoration button.titlebutton { min-height: 0; min-width: 0; padding: 0; margin: 0; border: none; }
headerbar.default-decoration label { min-height: 0; margin: 0; padding: 0; }";

/// Plain rules for the windows and widgets of a dialog, so it takes the colours even where GTK's theme does not build them
/// from the named colours alone (buttons and entries have gradients and borders of their own).
const WIDGET_CSS: &str = "window.background, dialog.background, .background { background-color: @theme_bg_color; color: @theme_fg_color; }
entry, spinbutton { background-image: none; background-color: @theme_base_color; color: @theme_text_color; border-color: @borders; box-shadow: none; }
entry:focus, spinbutton:focus { border-color: @theme_selected_bg_color; }
button { background-image: none; background-color: mix(@theme_bg_color, @theme_fg_color, 0.1); color: @theme_fg_color; border-color: @borders; box-shadow: none; text-shadow: none; }
button:hover { background-color: mix(@theme_bg_color, @theme_fg_color, 0.18); }
button.suggested-action, button.default { background-color: @theme_selected_bg_color; color: @theme_selected_fg_color; border-color: @theme_selected_bg_color; }
button.suggested-action:hover, button.default:hover { background-color: shade(@theme_selected_bg_color, 1.1); }";

fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The palette as the named colours GTK 3's built-in theme (Adwaita) is made from; everything else in the theme is derived
/// from these.
fn colors_css(p: &Palette) -> String {
    let colors = [
        ("theme_bg_color", p.surface),
        ("theme_fg_color", p.on_surface),
        ("theme_base_color", p.surface_container),
        ("theme_text_color", p.on_surface),
        ("theme_selected_bg_color", p.primary),
        ("theme_selected_fg_color", p.on_primary),
        ("theme_unfocused_bg_color", p.surface),
        ("theme_unfocused_fg_color", p.on_surface_variant),
        ("theme_unfocused_base_color", p.surface_container),
        ("theme_unfocused_text_color", p.on_surface_variant),
        ("theme_unfocused_selected_bg_color", p.primary_container),
        ("theme_unfocused_selected_fg_color", p.on_primary_container),
        ("insensitive_bg_color", p.surface_container),
        ("insensitive_fg_color", p.outline),
        ("insensitive_base_color", p.surface),
        ("borders", p.outline_variant),
        ("unfocused_borders", p.outline_variant),
        ("placeholder_text_color", p.on_surface_variant),
        ("content_view_bg", p.surface_container),
        ("text_view_bg", p.surface_container),
        ("link_color", p.primary),
        ("link_visited_color", p.secondary),
        ("error_color", p.error),
        ("warning_color", p.tertiary),
        ("success_color", p.secondary),
    ];
    colors.iter().map(|(name, c)| format!("@define-color {name} {};", hex(*c))).collect::<Vec<_>>().join("\n")
}

/// What goes between the markers; empty when there is nothing to set.
fn block_css(hide: bool, palette: Option<&Palette>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = palette {
        parts.push(colors_css(p));
        parts.push(WIDGET_CSS.to_string());
    }
    if hide {
        parts.push(TITLEBAR_CSS.to_string());
    }
    parts.join("\n")
}

/// `css` with our block replaced by `block` (or removed when `block` is empty). Everything outside it stays as it is.
fn with_block(css: &str, block: &str) -> String {
    let mut rest = css.to_string();
    for begin in [BEGIN, OLD_BEGIN] {
        if let (Some(a), Some(b)) = (rest.find(begin), rest.find(END)) {
            if b > a {
                rest = format!("{}{}", rest[..a].trim_end(), &rest[b + END.len()..]);
            }
        }
    }
    let rest = rest.trim().to_string();
    let rest = if rest.is_empty() { String::new() } else { format!("{rest}\n\n") };
    if !block.is_empty() {
        format!("{rest}{BEGIN}\n{block}\n{END}\n")
    } else if rest.is_empty() {
        String::new()
    } else {
        rest.trim_end().to_string() + "\n"
    }
}

/// Set (or stop setting) the look of GTK 3 apps for this user.
pub fn apply(hide: bool) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    let palette = ThemeSettings::load().gtk.then(Palette::load);
    let block = block_css(hide, palette.as_ref());
    apply_in(&base, &block, hide, true);
    // A flatpak app reads its own config folder, not `~/.config`: give each one the same files.
    if let Ok(apps) = fs::read_dir(home.join(".var/app")) {
        for app in apps.flatten() {
            apply_in(&app.path().join("config"), &block, hide, false);
        }
    }
}

/// Once a second: when the wallpaper colours or the colour settings changed, write the files again.
pub fn tick(hide: bool) {
    static LAST: Mutex<Option<(Option<SystemTime>, Option<SystemTime>)>> = Mutex::new(None);
    let now = (Palette::stamp(), ThemeSettings::stamp());
    let changed = {
        let Ok(mut last) = LAST.lock() else { return };
        let was = last.replace(now); // the first call only notes the state: the session start has just written the files
        matches!(was, Some(w) if w != now)
    };
    if changed {
        apply(hide);
    }
}

/// The files in `<config>/gtk-3.0`. `create_config`: make `config` itself if it is missing (otherwise it is skipped).
fn apply_in(config: &std::path::Path, block: &str, hide: bool, create_config: bool) {
    if !create_config && !config.is_dir() {
        return;
    }
    let dir = config.join("gtk-3.0");
    let css = dir.join("gtk.css");
    let text = fs::read_to_string(&css).unwrap_or_default();
    write_or_remove(&css, &text, with_block(&text, block), config);
    let ini = dir.join("settings.ini");
    let text = fs::read_to_string(&ini).unwrap_or_default();
    write_or_remove(&ini, &text, ini_with_layout(&text, hide), config);
}

/// `settings.ini` with `gtk-decoration-layout=:` (no window buttons: the titlebar then has nothing in it to keep it
/// tall, and the CSS above shrinks it to nothing) or without it. Off only removes an *empty* layout, which is ours.
fn ini_with_layout(text: &str, on: bool) -> String {
    let is_layout = |l: &str| l.trim().starts_with("gtk-decoration-layout");
    let ours = |l: &str| l.split_once('=').is_some_and(|(_, v)| v.trim().is_empty() || v.trim() == ":");
    let mut lines: Vec<String> = text.lines().filter(|l| !(is_layout(l) && (on || ours(l)))).map(str::to_owned).collect();
    if on {
        match lines.iter().position(|l| l.trim() == "[Settings]") {
            Some(i) => lines.insert(i + 1, "gtk-decoration-layout=:".into()),
            None => {
                if lines.last().is_some_and(|l| !l.is_empty()) {
                    lines.push(String::new());
                }
                lines.push("[Settings]".into());
                lines.push("gtk-decoration-layout=:".into());
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
        assert!(on.contains("gtk-decoration-layout=:\n") && on.contains("gtk-cursor-theme-name=Bibata"));
        assert_eq!(ini_with_layout(&on, true), on, "applying twice changes nothing");
        assert_eq!(ini_with_layout(&on, false), "[Settings]\ngtk-cursor-theme-name=Bibata\n");
        assert_eq!(ini_with_layout("", true), "[Settings]\ngtk-decoration-layout=:\n");
        assert_eq!(ini_with_layout(&ini_with_layout("", true), false), "");
        // a layout the user chose stays when we switch off
        assert_eq!(ini_with_layout("[Settings]\ngtk-decoration-layout=menu:close\n", false), "[Settings]\ngtk-decoration-layout=menu:close\n");
    }

    #[test]
    fn block_is_added_replaced_and_removed() {
        let mine = "button { color: red; }\n";
        let on = with_block(mine, "a {}");
        assert!(on.starts_with(mine.trim_end()) && on.contains(BEGIN) && on.contains(END) && on.contains("a {}"));
        assert_eq!(with_block(&on, "a {}"), on, "applying twice changes nothing");
        assert!(with_block(&on, "b {}").contains("b {}") && !with_block(&on, "b {}").contains("a {}"), "a new block replaces the old");
        assert_eq!(with_block(&on, ""), mine);
        assert_eq!(with_block("", ""), "");
        assert_eq!(with_block(&with_block("", "a {}"), ""), "");
        // the block's old name is replaced, not duplicated
        let old = format!("{OLD_BEGIN}\nold {{}}\n{END}\n");
        let new = with_block(&old, "a {}");
        assert!(new.contains(BEGIN) && !new.contains(OLD_BEGIN) && !new.contains("old {}"));
    }

    #[test]
    fn colours_follow_the_palette() {
        let css = block_css(false, Some(&Palette::default()));
        assert!(css.contains("@define-color theme_bg_color #14141a;"));
        assert!(css.contains("@define-color theme_selected_bg_color #8b7bff;"));
        assert!(css.contains("button.suggested-action"));
        assert!(!css.contains("default-decoration"));
        assert!(block_css(true, None).contains("default-decoration"));
        assert_eq!(block_css(false, None), "");
    }
}

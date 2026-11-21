//! Titlebars that clients draw themselves. Vesper asks every client for server-side decoration (and draws none), which
//! most toolkits honour. GTK 3 does not: Tauri apps (Astralis, flatpak or not) get GTK's own fallback titlebar. That one is
//! hidden through a block in `~/.config/gtk-3.0/gtk.css`, which only matches GTK's *default* titlebar (`default-decoration`),
//! never a header bar an app builds itself with its own buttons.

use std::{fs, path::PathBuf};

const BEGIN: &str = "/* vesper: hide default titlebars (Settings > Appearance) */";
const END: &str = "/* vesper: end */";
const CSS: &str = "headerbar.default-decoration, .titlebar.default-decoration {
    min-height: 0; padding: 0; margin: 0; border: none; box-shadow: none; background: none; font-size: 0;
}
.default-decoration button.titlebutton { min-height: 0; min-width: 0; padding: 0; margin: 0; border: none; }";

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
    let path = base.join("gtk-3.0").join("gtk.css");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let updated = with_block(&text, hide);
    if updated != text {
        if updated.is_empty() {
            let _ = fs::remove_file(&path);
        } else {
            let _ = fs::create_dir_all(path.parent().unwrap_or(&base));
            let _ = fs::write(&path, updated);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

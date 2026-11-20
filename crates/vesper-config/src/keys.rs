//! The rebindable keyboard shortcuts: one table of ids, labels and default keys, shared by the compositor (which acts on
//! them) and Settings (which edits them). Overrides live in `[keys]` in the config as `id = "Mod+Shift+Q"`; several keys
//! for one action are separated by commas, and an empty value means the default.

/// One shortcut the user may change.
pub struct Bind {
    pub id: &'static str,
    pub section: &'static str,
    pub label: &'static str,
    /// Default keys, comma separated. `Mod` is the configured shortcut key (Super or Alt).
    pub default: &'static str,
}

const fn b(section: &'static str, id: &'static str, label: &'static str, default: &'static str) -> Bind {
    Bind { id, section, label, default }
}

pub const BINDS: &[Bind] = &[
    b("Applications", "terminal", "Terminal", "Mod+Return"),
    b("Applications", "launcher", "App launcher", "Mod+Space"),
    b("Applications", "files", "File manager", "Mod+E"),
    b("Applications", "settings", "Settings", "Mod+,"),
    b("Applications", "blur", "Blur the focused window's app", "Mod+B"),
    b("Applications", "wallpapers", "Wallpaper picker", "Mod+W"),
    b("Applications", "notifications", "Notification centre", "Mod+N"),
    b("Applications", "power_menu", "Power menu", "Mod+Shift+P"),
    b("Windows", "close", "Close window", "Mod+Q"),
    b("Windows", "focus_left", "Focus the column on the left", "Mod+Left, Mod+H"),
    b("Windows", "focus_right", "Focus the column on the right", "Mod+Right"),
    b("Windows", "workspace_up", "Go to the workspace above", "Mod+Up, Mod+K"),
    b("Windows", "workspace_down", "Go to the workspace below", "Mod+Down, Mod+J"),
    b("Windows", "move_left", "Move the column left", "Mod+Shift+Left, Mod+Shift+H"),
    b("Windows", "move_right", "Move the column right", "Mod+Shift+Right, Mod+Shift+L"),
    b("Windows", "send_up", "Send the window to the workspace above", "Mod+Shift+Up, Mod+Shift+K"),
    b("Windows", "send_down", "Send the window to the workspace below", "Mod+Shift+Down, Mod+Shift+J"),
    b("Windows", "stack", "Stack into the column on the left", "Mod+["),
    b("Windows", "unstack", "Take the window out of its column", "Mod+]"),
    b("Windows", "column_up", "Focus up inside a column", "Mod+Alt+Up"),
    b("Windows", "column_down", "Focus down inside a column", "Mod+Alt+Down"),
    b("Windows", "cycle_width", "Cycle column width", "Mod+R"),
    b("Windows", "narrower", "Make a column narrower", "Mod+-"),
    b("Windows", "wider", "Make a column wider", "Mod+="),
    b("Windows", "fullscreen", "Fullscreen", "Mod+F"),
    b("Windows", "float", "Floating workspace (all windows float, or tile again)", "Mod+V"),
    b("Windows", "float_window", "Float or tile the focused window", "Mod+Shift+F"),
    b("Windows", "minimize", "Minimize", "Mod+M"),
    b("Windows", "restore", "Restore the last minimized window", "Mod+Shift+M"),
    b("System", "lock", "Lock the screen", "Mod+L, Ctrl+Alt+L"),
    b("System", "start_menu", "Open the start menu", "Mod+Escape"),
    b("System", "switcher", "Switch windows", "Mod+Tab, Mod+Shift+Tab"),
    b("System", "clipboard", "Clipboard history", "Mod+Shift+V"),
    b("System", "emoji", "Emoji picker", "Mod+."),
    b("System", "shot_screen", "Screenshot: whole screen", "Print"),
    b("System", "shot_window", "Screenshot: window", "Alt+Print"),
    b("System", "shot_region", "Screenshot: region", "Shift+Print, Mod+Shift+S"),
    b("System", "quit", "Quit Vesper", "Mod+Shift+E"),
];

/// One key combination.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Combo {
    /// The shortcut key (Super or Alt, whichever is configured).
    pub modk: bool,
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    /// Key name as xkb knows it ("Return", "q", "Left", "Print"); a single character stands for itself.
    pub key: String,
}

impl Combo {
    /// `"Mod+Shift+Q"`. Modifier names are case-insensitive: mod, super (also logo, win), alt, ctrl (control), shift.
    pub fn parse(s: &str) -> Option<Combo> {
        let mut c = Combo::default();
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        // "Mod++" means Mod and the plus key: the empty part before the last is that plus
        let (key, mods) = match parts.as_slice() {
            [.., "", ""] => ("+", &parts[..parts.len() - 2]),
            [.., k] => (*k, &parts[..parts.len() - 1]),
            [] => return None,
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "mod" => c.modk = true,
                "super" | "logo" | "win" | "mod4" => c.logo = true,
                "alt" | "mod1" => c.alt = true,
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                _ => return None,
            }
        }
        if key.is_empty() {
            return None;
        }
        c.key = match key.to_ascii_lowercase().as_str() {
            "enter" => "Return".into(),
            "esc" => "Escape".into(),
            "plus" => "+".into(),
            _ => key.to_string(),
        };
        Some(c)
    }

    /// Text form, for showing.
    pub fn show(&self) -> String {
        let mut v = Vec::new();
        if self.modk {
            v.push("Mod".to_string());
        }
        if self.logo {
            v.push("Super".into());
        }
        if self.ctrl {
            v.push("Ctrl".into());
        }
        if self.alt {
            v.push("Alt".into());
        }
        if self.shift {
            v.push("Shift".into());
        }
        v.push(self.key.clone());
        v.join("+")
    }
}

/// A comma separated list of combinations (a lone comma key is written `Mod+,`: the part after the last `+` is the key).
pub fn parse_list(s: &str) -> Vec<Combo> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        // a comma ends one combination unless it is the key itself (right after a `+` or at the very start of one)
        if *ch == ',' && !(cur.trim().is_empty() || cur.trim_end().ends_with('+')) {
            out.extend(Combo::parse(&cur));
            cur.clear();
        } else {
            cur.push(*ch);
        }
        let _ = i;
    }
    out.extend(Combo::parse(&cur));
    out
}

/// The keys in effect for `id`: the user's if set (and valid), else the default.
pub fn effective(id: &str, user: &std::collections::BTreeMap<String, String>) -> Vec<Combo> {
    let Some(bind) = BINDS.iter().find(|b| b.id == id) else { return Vec::new() };
    let mine = user.get(id).map(|s| parse_list(s)).unwrap_or_default();
    if mine.is_empty() { parse_list(bind.default) } else { mine }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combos() {
        let c = Combo::parse("Mod+Shift+Q").unwrap();
        assert!(c.modk && c.shift && !c.ctrl && c.key == "Q");
        assert_eq!(Combo::parse("super+enter").unwrap().key, "Return");
        assert_eq!(Combo::parse("Mod++").unwrap().key, "+");
        assert_eq!(Combo::parse("Print").unwrap(), Combo { key: "Print".into(), ..Default::default() });
        assert!(Combo::parse("Hyper+Q").is_none());
        assert!(Combo::parse("").is_none());
    }

    #[test]
    fn lists_keep_comma_keys() {
        let l = parse_list("Mod+,, Mod+Shift+Comma");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].key, ",");
        assert_eq!(l[1].key, "Comma");
        assert_eq!(parse_list("Mod+Left, Mod+H").len(), 2);
    }

    #[test]
    fn every_default_parses_and_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BINDS {
            assert!(seen.insert(b.id), "duplicate id {}", b.id);
            assert!(!parse_list(b.default).is_empty(), "{} default", b.id);
        }
    }

    #[test]
    fn user_keys_replace_the_default() {
        let mut m = std::collections::BTreeMap::new();
        assert_eq!(effective("close", &m)[0].key, "Q");
        m.insert("close".to_string(), "Mod+X".to_string());
        assert_eq!(effective("close", &m)[0].key, "X");
        m.insert("close".to_string(), "nonsense+".to_string());
        assert_eq!(effective("close", &m)[0].key, "Q");
    }
}

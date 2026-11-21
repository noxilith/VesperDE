//! Minimal freedesktop .desktop file index: enough to map an app id to a name, icon and launch command.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct DesktopEntry {
    /// File stem, e.g. `org.gnome.Nautilus`.
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub wm_class: String,
    /// `X-Flatpak=`: the flatpak application id, for apps installed through flatpak.
    pub flatpak: String,
    /// `NoDisplay=true` or `Hidden=true`: not meant to be listed in menus or launchers.
    pub no_display: bool,
    /// `Terminal=true`: must be run inside a terminal emulator.
    pub terminal: bool,
    /// The `Categories=` of the desktop file, for example `Network`, `Game`.
    pub categories: Vec<String>,
}

#[derive(Default)]
pub struct Apps {
    entries: Vec<DesktopEntry>,
    by_key: HashMap<String, usize>,
    /// Weaker keys (the last part of a reverse-DNS id: `claude` for `com.anthropic.Claude`), tried only when nothing else matches.
    by_loose: HashMap<String, usize>,
}

pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => dirs.push(PathBuf::from(d)),
        _ => {
            if let Some(h) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(h).join(".local/share"));
            }
        }
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let system = if system.is_empty() { "/usr/local/share:/usr/share".to_owned() } else { system };
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs
}

impl Apps {
    pub fn load() -> Self {
        let mut apps = Apps::default();
        // Earlier directories win, so a user's own entry overrides the system one.
        for dir in data_dirs() {
            apps.scan(&dir.join("applications"));
        }
        apps
    }

    fn scan(&mut self, dir: &Path) {
        let Ok(read) = fs::read_dir(dir) else { return };
        let mut files: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            if path.is_dir() {
                self.scan(&path);
            } else if path.extension().is_some_and(|e| e == "desktop") {
                if let Some(entry) = parse(&path) {
                    self.add(entry);
                }
            }
        }
    }

    fn add(&mut self, entry: DesktopEntry) {
        let i = self.entries.len();
        // A flatpak's Exec is `flatpak run ...`, so name the app by its flatpak id and by the program it runs inside.
        let flatpak_command = entry.exec.split_whitespace().find_map(|w| w.strip_prefix("--command=")).unwrap_or("").to_lowercase();
        let mut keys = vec![entry.id.to_lowercase(), entry.wm_class.to_lowercase(), exec_name(&entry.exec), flatpak_command, entry.flatpak.to_lowercase()];
        keys.retain(|k| !k.is_empty());
        for k in keys {
            match self.by_key.get(&k) {
                None => {
                    self.by_key.insert(k, i);
                }
                // A hidden helper entry without an icon (a URL handler that runs the same program) must not
                // hide the real app: let the entry with an icon, or the visible one, have the key.
                // (`j == i` is this very entry, listed under the same key twice, and is not in `entries` yet.)
                Some(&j) if j != i && ((self.entries[j].icon.is_empty() && !entry.icon.is_empty()) || (self.entries[j].no_display && !entry.no_display)) => {
                    self.by_key.insert(k, i);
                }
                _ => {}
            }
        }
        let id = entry.id.strip_suffix(".desktop").unwrap_or(&entry.id);
        if let Some(last) = id.rsplit('.').next().map(str::to_lowercase).filter(|l| l.len() > 2 && id.contains('.')) {
            self.by_loose.entry(last).or_insert(i);
        }
        self.entries.push(entry);
    }

    /// Every entry found, in discovery order (user directories first).
    pub fn entries(&self) -> &[DesktopEntry] {
        &self.entries
    }

    /// Find the entry for an app id / desktop id / window class (case-insensitive).
    pub fn find(&self, key: &str) -> Option<&DesktopEntry> {
        let k = key.trim_end_matches(".desktop").to_lowercase();
        // The candidates, best match first. A hidden entry without an icon (a URL handler that runs the same program)
        // is only taken when nothing better matches.
        let last = k.rsplit('.').next().filter(|l| *l != k && l.len() > 2).map(str::to_owned);
        let candidates = [
            self.by_key.get(&k),
            self.by_key.get(&format!("{k}.desktop")),
            // `Claude` for the app `com.anthropic.Claude`, or `astralis` for `app.astralis.desktop`
            self.by_loose.get(&k),
            last.as_ref().and_then(|l| self.by_key.get(l)),
        ];
        let usable = |i: &&usize| !(self.entries[**i].no_display && self.entries[**i].icon.is_empty());
        let hits = candidates.iter().copied().flatten();
        hits.clone().find(usable).or_else(|| hits.clone().next()).map(|&i| &self.entries[i])
    }

    /// Icon names to try for a window of `app_id`, best first: the desktop entry's icon, then the id itself in a few spellings.
    pub fn icon_names(&self, app_id: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut push = |n: String| {
            if !n.is_empty() && !names.contains(&n) {
                names.push(n);
            }
        };
        if let Some(e) = self.find(app_id) {
            push(e.icon.clone());
            push(e.id.clone());
            push(e.flatpak.clone());
        }
        let id = app_id.trim_end_matches(".desktop");
        push(app_id.to_owned());
        push(app_id.to_lowercase());
        push(id.to_owned());
        push(id.rsplit('.').next().unwrap_or("").to_lowercase());
        names
    }
}

/// Basename of the program in an Exec line.
fn exec_name(exec: &str) -> String {
    exec.split_whitespace()
        .next()
        .and_then(|p| Path::new(p).file_name())
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn parse(path: &Path) -> Option<DesktopEntry> {
    let text = fs::read_to_string(path).ok()?;
    let mut e = DesktopEntry { id: path.file_stem()?.to_string_lossy().into_owned(), ..Default::default() };
    let (mut in_entry, mut hidden, mut is_app) = (false, false, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "Name" => e.name = v.to_owned(),
            "Exec" => e.exec = strip_field_codes(v),
            "Icon" => e.icon = v.to_owned(),
            "StartupWMClass" => e.wm_class = v.to_owned(),
            "X-Flatpak" => e.flatpak = v.to_owned(),
            "Type" => is_app = v == "Application",
            "Hidden" | "NoDisplay" => hidden |= v == "true",
            "Terminal" => e.terminal = v == "true",
            "Categories" => e.categories = v.split(';').filter(|c| !c.is_empty()).map(str::to_owned).collect(),
            _ => {}
        }
    }
    e.no_display = hidden;
    (is_app && !e.exec.is_empty()).then_some(e)
}

fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') => out.push('%'),
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_codes() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("sh -c 'echo 100%%'"), "sh -c 'echo 100%'");
    }

    #[test]
    fn an_entry_named_like_its_program_is_indexed() {
        // id and Exec give the same key twice: the second must not look up an entry that is not stored yet
        let mut apps = Apps::default();
        apps.add(DesktopEntry { id: "kitty".into(), name: "Kitty".into(), exec: "kitty".into(), ..Default::default() });
        apps.add(DesktopEntry { id: "claude-code-url-handler".into(), exec: "claude".into(), no_display: true, ..Default::default() });
        apps.add(DesktopEntry { id: "com.anthropic.Claude".into(), wm_class: "com.anthropic.Claude".into(), exec: "claude-desktop".into(), icon: "claude-desktop".into(), ..Default::default() });
        assert_eq!(apps.find("kitty").map(|e| e.name.as_str()), Some("Kitty"));
        assert_eq!(apps.find("com.anthropic.Claude").map(|e| e.icon.as_str()), Some("claude-desktop"));
        assert_eq!(apps.find("Claude").map(|e| e.icon.as_str()), Some("claude-desktop"));
    }

    #[test]
    fn exec_basename() {
        assert_eq!(exec_name("/usr/bin/Kitty --single"), "kitty");
    }
}

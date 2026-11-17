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
        let mut keys = vec![entry.id.to_lowercase(), entry.wm_class.to_lowercase(), exec_name(&entry.exec)];
        keys.retain(|k| !k.is_empty());
        for k in keys {
            self.by_key.entry(k).or_insert(i);
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
        self.by_key.get(&k).or_else(|| self.by_key.get(&format!("{k}.desktop"))).map(|&i| &self.entries[i])
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
    fn exec_basename() {
        assert_eq!(exec_name("/usr/bin/Kitty --single"), "kitty");
    }
}

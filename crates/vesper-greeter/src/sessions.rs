//! The sessions you can log into (`wayland-sessions/*.desktop`) and what the login screen remembers between boots.

use std::{fs, path::PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub name: String,
    pub exec: String,
}

fn dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("VESPER_SESSIONS_DIR") {
        v.push(PathBuf::from(d));
    }
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    v.extend(data.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join("wayland-sessions")));
    v
}

pub fn parse(text: &str) -> Option<Session> {
    let (mut name, mut exec, mut hidden) = (None, None, false);
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry {
            if let Some(v) = line.strip_prefix("Name=") {
                name.get_or_insert_with(|| v.to_string());
            } else if let Some(v) = line.strip_prefix("Exec=") {
                exec.get_or_insert_with(|| v.to_string());
            } else if line == "NoDisplay=true" || line == "Hidden=true" {
                hidden = true;
            }
        }
    }
    if hidden {
        return None;
    }
    Some(Session { name: name?, exec: exec? })
}

/// All sessions found, Vesper first, then by name.
pub fn list() -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    for d in dirs() {
        let Ok(rd) = fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "desktop") {
                if let Some(s) = fs::read_to_string(e.path()).ok().and_then(|t| parse(&t)) {
                    if !out.iter().any(|o| o.exec == s.exec) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| (!s.name.to_lowercase().contains("vesper"), s.name.to_lowercase()));
    if out.is_empty() {
        out.push(Session { name: "Vesper".into(), exec: "vesper-session".into() });
    }
    out
}

fn cache_file() -> PathBuf {
    PathBuf::from(std::env::var_os("VESPER_GREETER_CACHE").unwrap_or_else(|| "/var/cache/vesper-greeter".into())).join("last")
}

/// The user and session name used last time.
pub fn last() -> (String, String) {
    let text = fs::read_to_string(cache_file()).unwrap_or_default();
    let mut l = text.lines();
    (l.next().unwrap_or_default().to_string(), l.next().unwrap_or_default().to_string())
}

pub fn remember(user: &str, session: &str) {
    let f = cache_file();
    if let Some(dir) = f.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(f, format!("{user}\n{session}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_session_file() {
        let s = parse("[Desktop Entry]\nName=Vesper\nExec=vesper-session\nType=Application\n").unwrap();
        assert_eq!((s.name.as_str(), s.exec.as_str()), ("Vesper", "vesper-session"));
        assert!(parse("[Desktop Entry]\nName=Old\nExec=old\nNoDisplay=true\n").is_none());
    }
}

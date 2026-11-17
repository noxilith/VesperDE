//! Config profiles: named copies of the whole configuration that you can keep several of, switch between, export to
//! share and import from other people. They live in `profiles/` next to the config file.

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::Config;

/// The folder holding the profiles.
pub fn dir() -> PathBuf {
    Config::user_file().parent().unwrap_or(Path::new(".")).join("profiles")
}

fn path_of(name: &str) -> PathBuf {
    dir().join(format!("{name}.toml"))
}

/// A usable profile name: letters, digits, spaces, `-`, `_` and `.`, at most 40 characters, not starting with a dot.
pub fn clean_name(s: &str) -> Option<String> {
    let cleaned: String = s.trim().chars().filter(|c| c.is_alphanumeric() || " -_.".contains(*c)).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').chars().take(40).collect::<String>().trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub modified: Option<SystemTime>,
    /// The one most recently saved or applied.
    pub active: bool,
}

/// The name of the profile most recently saved or applied.
pub fn active() -> Option<String> {
    fs::read_to_string(dir().join(".active")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn set_active(name: &str) {
    let _ = fs::write(dir().join(".active"), name);
}

/// Every saved profile, by name.
pub fn list() -> Vec<Profile> {
    let active = active();
    let mut v: Vec<Profile> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            (p.extension().is_some_and(|x| x == "toml") && !name.starts_with('.')).then(|| Profile {
                active: active.as_deref() == Some(name.as_str()),
                modified: e.metadata().and_then(|m| m.modified()).ok(),
                name,
            })
        })
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Is `text` a config Vesper can load? Returns what is wrong if not.
pub fn validate(text: &str) -> Result<(), String> {
    toml::from_str::<Config>(text).map(|_| ()).map_err(|e| e.message().to_string())
}

/// Save the config in use as profile `name` (replacing one of that name). Returns the name actually used.
pub fn save(name: &str) -> Result<String, String> {
    let name = clean_name(name).ok_or("Give the profile a name")?;
    let text = fs::read_to_string(Config::user_file()).unwrap_or_default();
    write_atomic(&path_of(&name), &text)?;
    set_active(&name);
    Ok(name)
}

/// Make profile `name` the config in use. What was in use is kept as the profile "Previous" so you can go back.
pub fn apply(name: &str) -> Result<(), String> {
    let text = fs::read_to_string(path_of(name)).map_err(|_| format!("No profile called {name}"))?;
    validate(&text).map_err(|e| format!("{name} is not a valid config: {e}"))?;
    let current = fs::read_to_string(Config::user_file()).unwrap_or_default();
    if current != text && name != "Previous" {
        write_atomic(&path_of("Previous"), &current)?;
    }
    write_atomic(&Config::user_file(), &text)?;
    set_active(name);
    Ok(())
}

pub fn delete(name: &str) -> Result<(), String> {
    fs::remove_file(path_of(name)).map_err(|e| e.to_string())?;
    if active().as_deref() == Some(name) {
        let _ = fs::remove_file(dir().join(".active"));
    }
    Ok(())
}

/// Remove what only makes sense on your own machine (monitors, wallpaper folders, autostart programs, pinned apps,
/// a custom logo path), so the config can be shared.
pub fn portable(text: &str) -> Result<String, String> {
    let mut t: toml::Table = toml::from_str(text).map_err(|e| e.message().to_string())?;
    t.remove("display");
    t.remove("autostart");
    for (section, keys) in [("wallpaper", &["path", "dir"][..]), ("launcher", &["pinned"][..])] {
        if let Some(toml::Value::Table(s)) = t.get_mut(section) {
            for k in keys {
                s.remove(*k);
            }
        }
    }
    if let Some(toml::Value::Table(bar)) = t.get_mut("bar") {
        if bar.get("system_icon").and_then(|v| v.as_str()).is_some_and(|s| s.starts_with('/') || s.starts_with('~')) {
            bar.remove("system_icon");
        }
    }
    t.retain(|_, v| !matches!(v, toml::Value::Table(s) if s.is_empty()));
    toml::to_string_pretty(&t).map_err(|e| e.to_string())
}

/// The text to share for profile `name` (machine-specific parts removed).
pub fn export_text(name: &str) -> Result<String, String> {
    let text = fs::read_to_string(path_of(name)).map_err(|_| format!("No profile called {name}"))?;
    let body = portable(&text)?;
    Ok(format!("# Vesper config profile: {name}\n# Import it in Settings > Profiles (or drop it into ~/.config/vesper/profiles).\n\n{body}"))
}

/// Write the shareable text of `name` to `file`.
pub fn export_to(name: &str, file: &Path) -> Result<(), String> {
    write_atomic_any(file, &export_text(name)?)
}

fn write_atomic_any(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    fs::write(path, text).map_err(|e| e.to_string())
}

/// Add the config file at `path` as a profile (named after the file, or "name-2" if that is taken). It is only added:
/// apply it to use it. Returns the profile's name.
pub fn import_file(path: &str) -> Result<String, String> {
    let p = if let Some(rest) = path.trim().strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(path.trim())
    };
    let text = fs::read_to_string(&p).map_err(|e| format!("Cannot read {}: {e}", p.display()))?;
    validate(&text).map_err(|e| format!("That is not a Vesper config: {e}"))?;
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let base = clean_name(&stem).unwrap_or_else(|| "Imported".into());
    let mut name = base.clone();
    let mut n = 2;
    while path_of(&name).exists() {
        name = format!("{base}-{n}");
        n += 1;
    }
    write_atomic(&path_of(&name), &text)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, in order, because it points the whole config at a temporary folder.
    #[test]
    fn profiles_round_trip() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("vesper-profiles-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::env::set_var("VESPER_CONFIG", &cfg);
        fs::write(&cfg, "[bar]\nradius = 20\nsystem_icon = \"/home/me/logo.svg\"\n\n[display]\nprimary = \"eDP-1\"\n\n[wallpaper]\npath = \"/home/me/w.png\"\n").unwrap();

        assert_eq!(clean_name("  My look! ").as_deref(), Some("My look"));
        assert_eq!(clean_name("../../etc").as_deref(), Some("etc"));
        assert_eq!(clean_name("!!!"), None);

        assert_eq!(save("Night").unwrap(), "Night");
        assert_eq!(list().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Night"]);
        assert_eq!(active().as_deref(), Some("Night"));

        // the shareable text drops what is personal and still loads
        let shared = export_text("Night").unwrap();
        assert!(shared.contains("radius = 20") && !shared.contains("eDP-1") && !shared.contains("w.png") && !shared.contains("logo.svg"));
        let file = dir.join("shared.toml");
        export_to("Night", &file).unwrap();
        let imported = import_file(file.to_str().unwrap()).unwrap();
        assert_eq!(imported, "shared");
        assert_eq!(import_file(file.to_str().unwrap()).unwrap(), "shared-2");

        // applying keeps what was in use as "Previous"
        fs::write(&cfg, "[bar]\nradius = 5\n").unwrap();
        apply("shared").unwrap();
        assert!(fs::read_to_string(&cfg).unwrap().contains("radius = 20"));
        assert!(fs::read_to_string(path_of("Previous")).unwrap().contains("radius = 5"));
        assert_eq!(active().as_deref(), Some("shared"));

        // junk is refused, and nothing changes
        fs::write(dir.join("bad.toml"), "[bar]\nnot_a_setting = 1\n").unwrap();
        assert!(import_file(dir.join("bad.toml").to_str().unwrap()).is_err());
        assert!(apply("missing").is_err());

        delete("shared-2").unwrap();
        assert!(!list().iter().any(|p| p.name == "shared-2"));
        let _ = fs::remove_dir_all(&dir);
        std::env::remove_var("VESPER_CONFIG");
    }
}

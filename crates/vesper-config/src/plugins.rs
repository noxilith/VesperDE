//! Plugins: folders with a `plugin.toml` that add things to the desktop. A plugin can start a background program with
//! the session, add widgets to the top bar (the text comes from a command you give) and add actions to the launcher.
//! Plugins run with your permissions: only switch on ones you trust.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::Config;

/// The plugin API version this build understands (`api` in `plugin.toml`).
pub const API: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub license: String,
    pub homepage: String,
    /// The plugin API version it was written for.
    pub api: u32,
    /// A program started with the session while the plugin is enabled (run in the plugin's folder).
    pub run: String,
    pub bar_item: Vec<BarItem>,
    pub action: Vec<Action>,
}

/// A widget for the top bar. `command` is run every `interval` seconds and its first line of output is shown; an empty
/// output hides the widget. A line like `icon|text` or `\u{f0e7}|text` draws a glyph before the text.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BarItem {
    pub id: String,
    /// The name shown in Settings > Bar > Items.
    pub label: String,
    pub command: String,
    pub interval: u32,
    /// Run when the widget is clicked.
    pub on_click: String,
}

/// An entry in the launcher's app list.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Action {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    /// The folder name: what `plugins.enabled` lists.
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub enabled: bool,
    /// It was written for a newer API than this build knows.
    pub too_new: bool,
}

/// Folders that hold plugins: yours first, then the ones installed with Vesper.
pub fn dirs() -> Vec<PathBuf> {
    let mut v = vec![user_dir()];
    v.push("/usr/share/vesper/plugins".into());
    if let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(exe.join("../../plugins")); // a dev build next to the source tree
    }
    v
}

/// `~/.config/vesper/plugins`: where Settings installs plugins.
pub fn user_dir() -> PathBuf {
    Config::user_file().parent().unwrap_or(Path::new(".")).join("plugins")
}

/// Read `plugin.toml` of a plugin folder.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(dir.join("plugin.toml")).map_err(|_| "no plugin.toml in that folder".to_string())?;
    let m: Manifest = toml::from_str(&text).map_err(|e| format!("plugin.toml: {}", e.message()))?;
    if m.name.trim().is_empty() {
        return Err("plugin.toml needs a name".into());
    }
    for b in &m.bar_item {
        if b.id.is_empty() || !b.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("a bar_item id must be letters, digits, - or _ (got {:?})", b.id));
        }
    }
    Ok(m)
}

/// Every plugin found, with whether it is switched on, sorted by name.
pub fn discover(enabled: &[String]) -> Vec<Plugin> {
    let mut out: Vec<Plugin> = Vec::new();
    for dir in dirs() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Some(id) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            if id.starts_with('.') || out.iter().any(|x| x.id == id) {
                continue; // the first plugin of a name wins (yours over the installed one)
            }
            let Ok(manifest) = read_manifest(&p) else { continue };
            out.push(Plugin { enabled: enabled.contains(&id), too_new: manifest.api > API, id, dir: p, manifest });
        }
    }
    out.sort_by_key(|p| p.manifest.name.to_lowercase());
    out
}

/// The plugins that are switched on and can run.
pub fn active(enabled: &[String]) -> Vec<Plugin> {
    discover(enabled).into_iter().filter(|p| p.enabled && !p.too_new).collect()
}

/// The id used for a plugin's bar widget in `bar.*_items`: `plugin:<folder>/<id>`.
pub fn item_id(plugin: &str, item: &str) -> String {
    format!("plugin:{plugin}/{item}")
}

/// Copy a plugin folder (or unpack a `.tar.gz`/`.tgz`) into your plugins folder. Returns its id. It is not switched on.
pub fn install(from: &str) -> Result<String, String> {
    let src = if let Some(rest) = from.trim().strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(from.trim())
    };
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("That is not a path")?;
    let dest_root = user_dir();
    fs::create_dir_all(&dest_root).map_err(|e| e.to_string())?;
    if src.is_dir() {
        let m = read_manifest(&src)?;
        let id = folder_id(&name, &m);
        let dest = dest_root.join(&id);
        if dest.exists() {
            return Err(format!("{id} is already installed (remove it first)"));
        }
        copy_dir(&src, &dest).map_err(|e| e.to_string())?;
        return Ok(id);
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let tmp = dest_root.join(format!(".unpack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let ok = std::process::Command::new("tar").arg("-xzf").arg(&src).arg("-C").arg(&tmp).status().is_ok_and(|s| s.success());
        let result = (|| {
            if !ok {
                return Err("Could not unpack it (is tar installed?)".to_string());
            }
            // the plugin is the folder with plugin.toml: the top folder of the archive, or the archive itself
            let top: Vec<PathBuf> = fs::read_dir(&tmp).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
            let plugin_dir = if tmp.join("plugin.toml").is_file() { tmp.clone() } else { top.into_iter().find(|p| p.join("plugin.toml").is_file()).ok_or("No plugin.toml in the archive")? };
            let m = read_manifest(&plugin_dir)?;
            let stem = name.trim_end_matches(".tar.gz").trim_end_matches(".tgz");
            let id = folder_id(stem, &m);
            let dest = dest_root.join(&id);
            if dest.exists() {
                return Err(format!("{id} is already installed (remove it first)"));
            }
            copy_dir(&plugin_dir, &dest).map_err(|e| e.to_string())?;
            Ok(id)
        })();
        let _ = fs::remove_dir_all(&tmp);
        return result;
    }
    Err("Give the path of a plugin folder or a .tar.gz file".into())
}

/// A safe folder name for a plugin: the folder's own name if it is a plain one, else made from the plugin's name.
fn folder_id(folder: &str, m: &Manifest) -> String {
    let clean = |s: &str| -> String {
        s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>().trim_matches('-').to_string()
    };
    let id = clean(folder);
    if id.is_empty() { clean(&m.name) } else { id }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_dir(&from, &to)?;
        } else if ty.is_symlink() {
            // links are followed only when they point at a file inside the plugin; others are skipped
            if let Ok(target) = fs::canonicalize(&from) {
                if target.starts_with(fs::canonicalize(src)?) && target.is_file() {
                    fs::copy(&target, &to)?;
                }
            }
        } else {
            fs::copy(&from, &to)?; // keeps the executable bit
        }
    }
    Ok(())
}

/// Delete a plugin you installed (the ones that come with Vesper cannot be removed).
pub fn remove(id: &str) -> Result<(), String> {
    if id.contains('/') || id.starts_with('.') || id.is_empty() {
        return Err("Not a plugin".into());
    }
    let dir = user_dir().join(id);
    if !dir.join("plugin.toml").is_file() {
        return Err("Only plugins in your own plugins folder can be removed".into());
    }
    fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

/// Switch a plugin on or off (in the config file).
pub fn set_enabled(id: &str, on: bool) -> std::io::Result<()> {
    let mut list = Config::load().plugins.enabled;
    list.retain(|p| p != id);
    if on {
        list.push(id.to_string());
    }
    Config::set("plugins.enabled", toml::Value::Array(list.into_iter().map(toml::Value::String).collect()))
}

/// The shell command that runs `command` inside the plugin's folder (so `./script.sh` works).
pub fn shell_in(dir: &Path, command: &str) -> String {
    format!("cd '{}' && {command}", dir.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_and_is_checked() {
        let d = std::env::temp_dir().join(format!("vesper-plugin-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("plugin.toml"),
            "name = \"Hello\"\nversion = \"0.1.0\"\napi = 1\nrun = \"./daemon.sh\"\n[[bar_item]]\nid = \"greeting\"\nlabel = \"Greeting\"\ncommand = \"./greet.sh\"\ninterval = 5\n[[action]]\nname = \"Say hello\"\ncommand = \"notify-send hi\"\n",
        )
        .unwrap();
        let m = read_manifest(&d).unwrap();
        assert_eq!((m.name.as_str(), m.api, m.bar_item.len(), m.action.len()), ("Hello", 1, 1, 1));
        assert_eq!(m.bar_item[0].interval, 5);
        fs::write(d.join("plugin.toml"), "name = \"X\"\n[[bar_item]]\nid = \"bad id!\"\n").unwrap();
        assert!(read_manifest(&d).is_err());
        fs::write(d.join("plugin.toml"), "version = \"1\"\n").unwrap();
        assert!(read_manifest(&d).unwrap_err().contains("name"));
        fs::write(d.join("plugin.toml"), "name = \"X\"\nbogus = 1\n").unwrap();
        assert!(read_manifest(&d).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    /// One test (it points the config at a temporary folder): install, find, enable, remove.
    #[test]
    fn install_discover_enable_remove() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("vesper-plugins-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/My Plugin")).unwrap();
        std::env::set_var("VESPER_CONFIG", root.join("config.toml"));
        fs::write(root.join("config.toml"), "").unwrap();
        fs::write(root.join("src/My Plugin/plugin.toml"), "name = \"My plugin\"\n").unwrap();
        fs::write(root.join("src/My Plugin/run.sh"), "#!/bin/sh\n").unwrap();

        let id = install(root.join("src/My Plugin").to_str().unwrap()).unwrap();
        assert_eq!(id, "my-plugin");
        assert!(install(root.join("src/My Plugin").to_str().unwrap()).is_err(), "twice");
        assert!(user_dir().join("my-plugin/run.sh").is_file());
        let found = discover(&[]);
        assert!(found.iter().any(|p| p.id == "my-plugin" && !p.enabled));

        set_enabled("my-plugin", true).unwrap();
        assert_eq!(Config::load().plugins.enabled, ["my-plugin"]);
        assert!(active(&Config::load().plugins.enabled).iter().any(|p| p.id == "my-plugin"));
        set_enabled("my-plugin", false).unwrap();
        assert!(Config::load().plugins.enabled.is_empty());

        assert!(install("/definitely/not/here").is_err());
        remove("my-plugin").unwrap();
        assert!(!user_dir().join("my-plugin").exists());
        assert!(remove("../x").is_err());
        assert_eq!(item_id("a", "b"), "plugin:a/b");
        assert_eq!(shell_in(Path::new("/tmp/it's"), "./x"), "cd '/tmp/it'\\''s' && ./x");
        let _ = fs::remove_dir_all(&root);
        std::env::remove_var("VESPER_CONFIG");
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;

    /// The starter plugin in `templates/` must stay valid.
    #[test]
    fn template_plugin_is_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/vesper-plugin-template");
        let m = read_manifest(&dir).unwrap();
        assert_eq!(m.api, API);
        assert_eq!(m.bar_item.len(), 1);
        assert_eq!(m.action.len(), 1);
        for f in ["status.sh", "clicked.sh", "daemon.sh", "package.sh"] {
            assert!(dir.join(f).is_file(), "{f} is missing");
        }
    }
}

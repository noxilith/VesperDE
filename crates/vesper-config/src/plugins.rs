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
pub const API: u32 = 2;

/// What a plugin may ask for in `permissions` (API 2). A plugin written for API 1 keeps its old rights: `run`, `bar` and `launcher`.
pub const PERMISSIONS: [&str; 9] = ["run", "bar", "launcher", "hooks", "keys", "commands", "config", "replace", "style"];

/// The programs of the desktop that a plugin may replace (`[[replace]] component = "launcher"`).
pub const COMPONENTS: [&str; 11] = ["bar", "launcher", "lock", "notify", "wallpaper", "wallpaper-picker", "switcher", "polkit", "screenshot", "power", "settings"];

/// Events a `[[hook]]` may name.
pub const EVENTS: [&str; 5] = ["window_opened", "window_closed", "window_focused", "workspace_changed", "output_changed"];

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
    /// What the plugin needs (API 2): see [`PERMISSIONS`]. Settings shows this list; features that are not listed do not run.
    pub permissions: Vec<String>,
    /// Commands run when something happens on the desktop (needs `hooks`).
    pub hook: Vec<Hook>,
    /// Keyboard shortcuts the plugin adds (needs `keys`); the user can change them in Settings.
    pub key: Vec<Key>,
    /// Search results the plugin adds to the launcher (needs `launcher`).
    pub launcher_source: Vec<Source>,
    /// A TOML file in the plugin folder with settings (the same keys as `config.toml`: `[bar]`, `[colors]`, `[launcher]` ...) that
    /// restyle the desktop while the plugin is on (needs `style`). The user's own config.toml still wins over it.
    pub style: String,
    /// Programs of the desktop this plugin replaces with its own (needs `replace`).
    pub replace: Vec<Replace>,
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

/// Run `command` when `event` happens. The environment has `VESPER_EVENT`, `VESPER_APP_ID`, `VESPER_TITLE` and (for workspace
/// events) `VESPER_WORKSPACE`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hook {
    pub event: String,
    pub command: String,
}

/// A shortcut: `default` is written like `Mod+Shift+G` (see Settings > Keyboard shortcuts).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Key {
    pub id: String,
    pub label: String,
    pub default: String,
    pub command: String,
}

/// Run `command` instead of the built-in program `component` (`bar`, `launcher`, `lock`, ...). It gets the same arguments the
/// built-in one would, and runs in the plugin folder. A replacement lock screen must speak ext-session-lock, a replacement bar
/// or launcher is a layer-shell client; they are used the next time the program starts (the launcher and lock screen: the next
/// time they open; the bar, notifications and wallpaper: the next login).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Replace {
    pub component: String,
    pub command: String,
}

/// A search provider for the launcher. `command` runs with the typed text in `VESPER_QUERY` and prints one result per line as
/// `title`, `title|description` or `title|description|command` (the command runs when it is picked; without one the title is run).
/// With a `prefix` (for example `calc`) it is asked only when the text starts with it, and the prefix is cut off.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Source {
    pub name: String,
    pub command: String,
    pub prefix: String,
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

impl Plugin {
    /// May this plugin use `feature` (one of [`PERMISSIONS`])? API 1 plugins have `run`, `bar` and `launcher`.
    pub fn can(&self, feature: &str) -> bool {
        if self.manifest.api < 2 {
            return matches!(feature, "run" | "bar" | "launcher");
        }
        self.manifest.permissions.iter().any(|p| p == feature)
    }
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
    for p in &m.permissions {
        if !PERMISSIONS.contains(&p.as_str()) {
            return Err(format!("unknown permission {p:?} (known: {})", PERMISSIONS.join(", ")));
        }
    }
    for h in &m.hook {
        if !EVENTS.contains(&h.event.as_str()) {
            return Err(format!("unknown hook event {:?} (known: {})", h.event, EVENTS.join(", ")));
        }
    }
    for r in &m.replace {
        if !COMPONENTS.contains(&r.component.trim_start_matches("vesper-")) {
            return Err(format!("cannot replace {:?} (known: {})", r.component, COMPONENTS.join(", ")));
        }
        if r.command.trim().is_empty() {
            return Err(format!("replace {:?} needs a command", r.component));
        }
    }
    if !m.style.is_empty() {
        let p = Path::new(&m.style);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("style must be a file inside the plugin folder".into());
        }
    }
    for k in &m.key {
        if k.id.is_empty() || !k.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("a key id must be letters, digits, - or _ (got {:?})", k.id));
        }
        if crate::keys::parse_list(&k.default).is_empty() {
            return Err(format!("key {:?}: '{}' is not a key combination", k.id, k.default));
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

    #[test]
    fn api2_permissions_hooks_keys() {
        let d = std::env::temp_dir().join(format!("vesper-plugin-api2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let write = |t: &str| fs::write(d.join("plugin.toml"), t).unwrap();
        write("name = \"P\"\napi = 2\npermissions = [\"hooks\", \"keys\"]\n[[hook]]\nevent = \"window_opened\"\ncommand = \"./a.sh\"\n[[key]]\nid = \"go\"\ndefault = \"Mod+Shift+G\"\ncommand = \"./go.sh\"\n[[launcher_source]]\nname = \"Calc\"\ncommand = \"./q.sh\"\nprefix = \"calc\"\n");
        let m = read_manifest(&d).unwrap();
        let p = Plugin { id: "p".into(), dir: d.clone(), enabled: true, too_new: false, manifest: m };
        assert!(p.can("hooks") && p.can("keys"));
        assert!(!p.can("launcher") && !p.can("run") && !p.can("bar"), "API 2 gets only what it lists");
        let old = Plugin { manifest: Manifest { api: 1, ..Default::default() }, ..p.clone() };
        assert!(old.can("run") && old.can("bar") && old.can("launcher") && !old.can("hooks"));
        write("name = \"P\"\napi = 2\npermissions = [\"root\"]\n");
        assert!(read_manifest(&d).unwrap_err().contains("permission"));
        write("name = \"P\"\napi = 2\n[[hook]]\nevent = \"sneeze\"\ncommand = \"x\"\n");
        assert!(read_manifest(&d).unwrap_err().contains("event"));
        write("name = \"P\"\napi = 2\n[[key]]\nid = \"a\"\ndefault = \"Hyper+Q\"\ncommand = \"x\"\n");
        assert!(read_manifest(&d).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn replace_and_style_manifest_and_merge() {
        let d = std::env::temp_dir().join(format!("vesper-plugin-style-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let write = |t: &str| fs::write(d.join("plugin.toml"), t).unwrap();
        write("name = \"T\"\napi = 2\npermissions = [\"replace\", \"style\"]\nstyle = \"look.toml\"\n[[replace]]\ncomponent = \"launcher\"\ncommand = \"./my-launcher\"\n");
        let m = read_manifest(&d).unwrap();
        let p = Plugin { id: "t".into(), dir: d.clone(), enabled: true, too_new: false, manifest: m };
        assert!(p.can("replace") && p.can("style"));
        write("name = \"T\"\napi = 2\n[[replace]]\ncomponent = \"kernel\"\ncommand = \"x\"\n");
        assert!(read_manifest(&d).is_err(), "unknown component");
        write("name = \"T\"\napi = 2\nstyle = \"../escape.toml\"\n");
        assert!(read_manifest(&d).is_err(), "style outside the folder");
        let mut base: toml::Table = "[bar]\nheight = 30\nopacity = 80\n[launcher]\nrows = 8\n".parse().unwrap();
        let top: toml::Table = "[bar]\nheight = 40\n".parse().unwrap();
        deep_merge(&mut base, &top);
        assert_eq!(base["bar"]["height"].as_integer(), Some(40));
        assert_eq!(base["bar"]["opacity"].as_integer(), Some(80), "keys not in the top layer stay");
        assert_eq!(base["launcher"]["rows"].as_integer(), Some(8));
        let _ = fs::remove_dir_all(&d);
    }

    /// A plugin's style file sits under the user's config, and a bad one is left out.
    #[test]
    fn style_layer_applies_under_the_user_config() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("vesper-style-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let pdir = root.join("plugins/looks");
        fs::create_dir_all(&pdir).unwrap();
        std::env::set_var("VESPER_CONFIG", root.join("config.toml"));
        fs::write(pdir.join("plugin.toml"), "name = \"Looks\"\napi = 2\npermissions = [\"style\"]\nstyle = \"look.toml\"\n").unwrap();
        fs::write(pdir.join("look.toml"), "[bar]\nheight = 44\nopacity = 70\n[launcher]\nrows = 11\n").unwrap();
        fs::write(root.join("config.toml"), "[plugins]\nenabled = [\"looks\"]\n[bar]\nheight = 36\n").unwrap();
        let cfg = Config::load();
        assert_eq!(cfg.bar.height, 36, "the user's own value wins");
        assert_eq!(cfg.bar.opacity, 70, "the plugin's value fills in");
        assert_eq!(cfg.launcher.rows, 11);
        fs::write(pdir.join("look.toml"), "[bar]\nnot_a_setting = 1\n").unwrap();
        assert_eq!(Config::load().bar.height, 36, "a bad style file is ignored, not fatal");
        let _ = fs::remove_dir_all(&root);
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
        for f in ["status.sh", "clicked.sh", "daemon.sh", "package.sh", "on-window.sh", "results.sh"] {
            assert!(dir.join(f).is_file(), "{f} is missing");
        }
    }
}

/// The config key under `[keys]` for a plugin's shortcut.
pub fn key_id(plugin: &str, key: &str) -> String {
    format!("plugin:{plugin}/{key}")
}

/// The programs enabled plugins (with the `replace` permission) stand in for: the full program name (`vesper-launcher`) with the
/// plugin that replaces it and the command to run. The first plugin (by name) for a program wins.
pub fn replacements(enabled: &[String]) -> Vec<(String, Plugin, String)> {
    let mut out: Vec<(String, Plugin, String)> = Vec::new();
    for p in active(enabled).into_iter().filter(|p| p.can("replace")) {
        for r in &p.manifest.replace {
            let name = format!("vesper-{}", r.component.trim_start_matches("vesper-"));
            if !out.iter().any(|(n, _, _)| *n == name) {
                out.push((name, p.clone(), r.command.clone()));
            }
        }
    }
    out
}

/// The `style` files of enabled plugins (with the `style` permission), as settings tables, in plugin order.
pub fn style_layers(enabled: &[String]) -> Vec<(String, toml::Table)> {
    let mut out = Vec::new();
    for p in active(enabled).into_iter().filter(|p| p.can("style") && !p.manifest.style.is_empty()) {
        let path = p.dir.join(&p.manifest.style);
        match fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| t.parse::<toml::Table>().map_err(|e| e.to_string())) {
            Ok(t) => out.push((p.id.clone(), t)),
            Err(e) => tracing::warn!("plugin {}: style file {}: {e}", p.id, path.display()),
        }
    }
    out
}

/// Put `top` over `base`: tables merge key by key, anything else is replaced.
pub fn deep_merge(base: &mut toml::Table, top: &toml::Table) {
    for (k, v) in top {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(t)) => deep_merge(b, t),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

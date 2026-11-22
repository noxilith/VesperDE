//! Colour theming with matugen.
//!
//! The flow: when the wallpaper changes, [`regenerate`] runs `matugen image` with Vesper's own matugen config. That config
//! always contains a built-in template that writes [`colors_file`], which every Vesper component reads as a [`Palette`]
//! (and reloads when it changes). Any templates you add run too, with their post-hooks, so other apps can follow the wallpaper.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

/// Name of the built-in template; it cannot be removed or edited from the settings.
pub const BUILTIN: &str = "vesper";

const BUILTIN_TEMPLATE: &str = include_str!("colors.template.toml");
const KITTY_TEMPLATE: &str = include_str!("kitty.template.conf");
const FOOT_TEMPLATE: &str = include_str!("foot.template.ini");
const SHELL_TEMPLATE: &str = include_str!("shell.template.sh");
const FETCH_TEMPLATE: &str = include_str!("fetch.template");
const FM_TEMPLATE: &str = include_str!("file-manager.template.json");
const VESKTOP_TEMPLATE: &str = include_str!("vesktop.template.css");

/// Built-in app templates (switched on and off in the settings), reserved so your own templates cannot clash.
pub const APP_TEMPLATES: [&str; 7] = ["vesper-kitty", "vesper-foot", "vesper-shell", "vesper-fetch", "vesper-file-manager", "vesper-vesktop", "vesper-vesktop-native"];

/// The matugen schemes, in the order the settings cycle through them.
pub const SCHEMES: [&str; 10] = [
    "scheme-tonal-spot",
    "scheme-vibrant",
    "scheme-expressive",
    "scheme-content",
    "scheme-fidelity",
    "scheme-fruit-salad",
    "scheme-rainbow",
    "scheme-neutral",
    "scheme-monochrome",
    "scheme-smart",
];

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config")).join("vesper")
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".local/state")).join("vesper")
}

/// Vesper's own matugen config (your `~/.config/matugen` is never touched).
pub fn matugen_config_file() -> PathBuf {
    config_dir().join("matugen.toml")
}

pub fn builtin_template_file() -> PathBuf {
    config_dir().join("templates/vesper-colors.toml")
}

/// Where the built-in template writes the palette that Vesper components read.
pub fn colors_file() -> PathBuf {
    state_dir().join("colors.toml")
}

pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if p == "~" => home(),
        None => PathBuf::from(p),
    }
}

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    /// Generate colours from the wallpaper with matugen.
    pub enabled: bool,
    /// "dark", "light" or "smart" (matugen picks from the image).
    pub mode: String,
    /// One of [`SCHEMES`].
    pub scheme: String,
    /// -1.0 (least contrast) to 1.0 (most).
    pub contrast: f32,
    /// Which colour to use when a wallpaper has several candidates: "saturation", "less-saturation", "darkness", "lightness" or "value".
    pub prefer: String,
    /// The matugen executable.
    pub binary: String,
    /// Colour kitty from the wallpaper (writes `~/.config/kitty/vesper-colors.conf` and includes it from `kitty.conf`).
    pub kitty: bool,
    /// Colour foot from the wallpaper (writes `~/.config/foot/vesper-colors.ini` and includes it from `foot.ini`).
    pub foot: bool,
    /// Write the prompt colours to `~/.local/state/vesper/shell-colors.sh` (the shell prompt reads it at every prompt).
    pub shell: bool,
    /// Colour fetch from the wallpaper (writes `~/.config/fetch/colors`, which fetch reads on every start).
    pub fetch: bool,
    /// Colour File Manager from the wallpaper (writes a `vesper` theme for it).
    pub file_manager: bool,
    /// Answer the ShellTint browser extension from Vesper's own palette (the built-in helper, `vesper-shelltint-host`). Off by default.
    pub shelltint: bool,
    /// Colour GTK 3 apps (the keyring password prompt, ...) from the wallpaper, through `~/.config/gtk-3.0/gtk.css`.
    pub gtk: bool,
    /// Write a Discord theme called "Vesper" for Vesktop (Vencord) with the wallpaper accent colour. Only if Vesktop is installed; you still turn it on in Vesktop under Themes.
    pub vesktop: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self { enabled: true, mode: "dark".into(), scheme: "scheme-smart".into(), contrast: 0.0, prefer: "saturation".into(), binary: "matugen".into(), kitty: true, foot: true, shell: true, fetch: true, file_manager: true, shelltint: false, gtk: true, vesktop: true }
    }
}

impl ThemeSettings {
    pub fn file() -> PathBuf {
        config_dir().join("theme.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::file()).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        write_atomic(&Self::file(), &format!("# Written by the wallpaper changer's Theme screen; safe to edit by hand.\n{}", toml::to_string(self).map_err(io::Error::other)?))
    }

    pub fn stamp() -> Option<std::time::SystemTime> {
        std::fs::metadata(Self::file()).and_then(|m| m.modified()).ok()
    }
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

// ------------------------------------------------------------------ templates

/// One matugen template: render `input_path` with the colours into `output_path`, then run `post_hook`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Template {
    pub name: String,
    pub input_path: String,
    pub output_path: String,
    pub post_hook: String,
}

fn parse_templates(text: &str) -> Vec<Template> {
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let Some(templates) = table.get("templates").and_then(|t| t.as_table()) else { return Vec::new() };
    let s = |t: &toml::Table, k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_owned();
    templates
        .iter()
        .filter_map(|(name, v)| {
            let t = v.as_table()?;
            Some(Template { name: name.clone(), input_path: s(t, "input_path"), output_path: s(t, "output_path"), post_hook: s(t, "post_hook") })
        })
        .collect()
}

/// Your own templates in Vesper's matugen config (not the built-in one).
pub fn read_templates() -> Vec<Template> {
    let text = std::fs::read_to_string(matugen_config_file()).unwrap_or_default();
    parse_templates(&text).into_iter().filter(|t| t.name != BUILTIN && !APP_TEMPLATES.contains(&t.name.as_str())).collect()
}

/// Templates defined in another matugen config (for example `~/.config/matugen/config.toml`), to import.
pub fn read_templates_from(path: &Path) -> Vec<Template> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    parse_templates(&text).into_iter().filter(|t| t.name != BUILTIN && !APP_TEMPLATES.contains(&t.name.as_str())).collect()
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// Write Vesper's matugen config: the built-in template first, then `user`'s templates. Also refreshes the built-in
/// template file itself, so it always matches this version of Vesper.
pub fn write_templates(user: &[Template]) -> io::Result<()> {
    write_atomic(&builtin_template_file(), BUILTIN_TEMPLATE)?;
    let mut out = String::from("# Vesper's matugen config. Written by the wallpaper changer's Theme screen; safe to edit by hand.\n# The [templates.vesper] entry is built in and is rewritten every time.\n\n[config]\n\n");
    out.push_str(&format!(
        "[templates.{BUILTIN}]\ninput_path = {}\noutput_path = {}\n",
        toml_string(&builtin_template_file().to_string_lossy()),
        toml_string(&colors_file().to_string_lossy()),
    ));
    let settings = ThemeSettings::load();
    if settings.kitty {
        write_atomic(&app_template_file("kitty.conf"), KITTY_TEMPLATE)?;
        out.push_str(&format!(
            "\n[templates.vesper-kitty]\ninput_path = {}\noutput_path = {}\npost_hook = {}\n",
            toml_string(&app_template_file("kitty.conf").to_string_lossy()),
            toml_string(&kitty_colors_file().to_string_lossy()),
            toml_string("kill -USR1 $(pidof kitty) 2>/dev/null || true"),
        ));
        link_kitty();
    }
    if settings.foot {
        write_atomic(&app_template_file("foot.ini"), FOOT_TEMPLATE)?;
        out.push_str(&format!(
            "\n[templates.vesper-foot]\ninput_path = {}\noutput_path = {}\npost_hook = {}\n",
            toml_string(&app_template_file("foot.ini").to_string_lossy()),
            toml_string(&foot_colors_file().to_string_lossy()),
            toml_string("pkill -USR1 -x foot || true"),
        ));
        link_foot();
    }
    if settings.shell {
        write_atomic(&app_template_file("shell-colors.sh"), SHELL_TEMPLATE)?;
        out.push_str(&format!(
            "\n[templates.vesper-shell]\ninput_path = {}\noutput_path = {}\n",
            toml_string(&app_template_file("shell-colors.sh").to_string_lossy()),
            toml_string(&shell_colors_file().to_string_lossy()),
        ));
    } else {
        let _ = std::fs::remove_file(shell_colors_file());
    }
    if settings.fetch {
        write_atomic(&app_template_file("fetch-colors"), FETCH_TEMPLATE)?;
        out.push_str(&format!(
            "\n[templates.vesper-fetch]\ninput_path = {}\noutput_path = {}\n",
            toml_string(&app_template_file("fetch-colors").to_string_lossy()),
            toml_string(&fetch_colors_file().to_string_lossy()),
        ));
    } else {
        let _ = std::fs::remove_file(fetch_colors_file());
    }
    if settings.file_manager {
        let first = !file_manager_theme_file().exists();
        write_atomic(&app_template_file("file-manager.json"), FM_TEMPLATE)?;
        out.push_str(&format!(
            "\n[templates.vesper-file-manager]\ninput_path = {}\noutput_path = {}\n",
            toml_string(&app_template_file("file-manager.json").to_string_lossy()),
            toml_string(&file_manager_theme_file().to_string_lossy()),
        ));
        if first {
            set_file_manager_theme(true);
        }
    }
    if settings.vesktop {
        let files = vesktop_theme_files();
        if !files.is_empty() {
            write_atomic(&app_template_file("vesktop.theme.css"), VESKTOP_TEMPLATE)?;
        }
        for (name, path) in files {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            out.push_str(&format!(
                "\n[templates.{name}]\ninput_path = {}\noutput_path = {}\n",
                toml_string(&app_template_file("vesktop.theme.css").to_string_lossy()),
                toml_string(&path.to_string_lossy()),
            ));
        }
    } else {
        for (_, path) in vesktop_theme_files() {
            let _ = std::fs::remove_file(path);
        }
    }
    for t in user.iter().filter(|t| t.name != BUILTIN && !APP_TEMPLATES.contains(&t.name.as_str()) && !t.name.is_empty()) {
        out.push_str(&format!("\n[templates.{}]\ninput_path = {}\noutput_path = {}\n", toml_string(&t.name), toml_string(&t.input_path), toml_string(&t.output_path)));
        if !t.post_hook.trim().is_empty() {
            out.push_str(&format!("post_hook = {}\n", toml_string(&t.post_hook)));
        }
    }
    write_atomic(&matugen_config_file(), &out)
}

/// Make sure the config and built-in template exist (first run), keeping any templates you already have.
pub fn ensure_files() -> io::Result<()> {
    write_templates(&read_templates())
}

// ------------------------------------------------------------------ running matugen

/// Find an executable the way you would from a shell, but without depending on this process's PATH: a tty or display-manager
/// session often has a shorter PATH than your desktop. Order: a path as given, PATH, the usual per-user and system folders,
/// then a probe of your login shell (which reads your own PATH setup).
pub fn find_binary(name: &str) -> Option<PathBuf> {
    let is_exec = |p: &Path| p.is_file() && std::fs::metadata(p).map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0).unwrap_or(false);
    if name.contains('/') {
        let p = expand_home(name);
        return is_exec(&p).then_some(p);
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for extra in [".local/bin", ".cargo/bin", ".local/share/omarchy/bin", ".nix-profile/bin", "bin"] {
        dirs.push(home().join(extra));
    }
    dirs.extend(["/usr/local/bin", "/usr/bin", "/bin", "/opt/bin"].map(PathBuf::from));
    if let Some(found) = dirs.iter().map(|d| d.join(name)).find(|p| is_exec(p)) {
        return Some(found);
    }
    // Last resort: ask your login shell, which knows the PATH you set up in your own profile.
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let out = Command::new(shell).args(["-l", "-c", &format!("command -v {name}")]).stdin(std::process::Stdio::null()).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout).lines().last()?.trim().to_owned();
    let p = PathBuf::from(line);
    (p.is_absolute() && is_exec(&p)).then_some(p)
}

/// Generate the colours for `image` and render every template. Blocks for about a second; call it off the UI thread.
pub fn regenerate(image: &Path, settings: &ThemeSettings) -> Result<(), String> {
    if !settings.enabled {
        // Theming is off: drop the generated colours so every component falls back to Vesper's own.
        let _ = std::fs::remove_file(colors_file());
        return Ok(());
    }
    ensure_files().map_err(|e| format!("could not write the matugen config: {e}"))?;
    let Some(binary) = find_binary(&settings.binary) else {
        return Err(format!(
            "{} was not found. Install matugen, or set `binary = \"/full/path/to/matugen\"` in {}",
            settings.binary,
            ThemeSettings::file().display()
        ));
    };
    let out = Command::new(&binary)
        .arg("image")
        .arg(image)
        .arg("-c")
        .arg(matugen_config_file())
        .args(["-m", &settings.mode, "-t", &settings.scheme])
        .args(["--contrast", &format!("{:.2}", settings.contrast.clamp(-1.0, 1.0))])
        .args(["--prefer", &settings.prefer])
        .arg("--continue-on-error")
        .output()
        .map_err(|e| format!("could not run {}: {e}", binary.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(clean_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// matugen's error output without colour codes, backtrace hints and blank lines, on one line.
pub fn clean_error(stderr: &str) -> String {
    let mut plain = String::new();
    let mut chars = stderr.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // skip an ANSI escape sequence: ESC [ ... letter
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    let parts: Vec<String> = plain
        .lines()
        .map(|l| l.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == ':').trim().to_owned())
        .filter(|l| !l.is_empty() && !l.starts_with("Error") && !l.contains("RUST_BACKTRACE") && !l.starts_with("Backtrace"))
        .collect();
    if parts.is_empty() { "matugen failed".into() } else { parts.join(" - ") }
}

// ------------------------------------------------------------------ the palette

pub type Rgba = [u8; 4];

/// The colours Vesper components use: Material You roles, as produced by matugen.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub surface: Rgba,
    pub surface_container: Rgba,
    pub surface_container_high: Rgba,
    pub surface_container_highest: Rgba,
    pub on_surface: Rgba,
    pub on_surface_variant: Rgba,
    pub outline: Rgba,
    pub outline_variant: Rgba,
    pub primary: Rgba,
    pub on_primary: Rgba,
    pub primary_container: Rgba,
    pub on_primary_container: Rgba,
    pub secondary: Rgba,
    pub tertiary: Rgba,
    pub error: Rgba,
    pub scrim: Rgba,
}

pub fn hex(s: &str) -> Option<Rgba> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 && h.len() != 8 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(if h.len() == 6 { [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255] } else { [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8] })
}

impl Default for Palette {
    /// Vesper's own dark look, used until matugen has produced colours (or when theming is off).
    fn default() -> Self {
        let c = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255];
        Self {
            surface: c(0x14141a),
            surface_container: c(0x1b1b23),
            surface_container_high: c(0x24242e),
            surface_container_highest: c(0x2d2d39),
            on_surface: c(0xe6e6ee),
            on_surface_variant: c(0x9a9aae),
            outline: c(0x6a6a7c),
            outline_variant: c(0x3a3a48),
            primary: c(0x8b7bff),
            on_primary: c(0x1a1040),
            primary_container: c(0x3a2f8f),
            on_primary_container: c(0xe3deff),
            secondary: c(0xb3a8ff),
            tertiary: c(0xffb4cf),
            error: c(0xff5f6d),
            scrim: c(0x000000),
        }
    }
}

impl Palette {
    pub fn file() -> PathBuf {
        colors_file()
    }

    /// The palette matugen last generated, falling back to the default for anything missing.
    pub fn load() -> Self {
        let mut p = Palette::from_file();
        // colours the user set in `[colors]` win over the wallpaper
        let mine = crate::Config::load().colors;
        macro_rules! over {
            ($($f:ident),*) => { $( if let Some(c) = mine.get(stringify!($f)).and_then(|s| hex(s)) { p.$f = c; } )* };
        }
        over!(
            surface, surface_container, surface_container_high, surface_container_highest, on_surface, on_surface_variant,
            outline, outline_variant, primary, on_primary, primary_container, on_primary_container, secondary, tertiary,
            error, scrim
        );
        p
    }

    fn from_file() -> Self {
        let mut p = Palette::default();
        let Ok(text) = std::fs::read_to_string(colors_file()) else { return p };
        let Ok(t) = text.parse::<toml::Table>() else { return p };
        let get = |k: &str| t.get(k).and_then(|v| v.as_str()).and_then(hex);
        macro_rules! role {
            ($($f:ident),*) => { $( if let Some(c) = get(stringify!($f)) { p.$f = c; } )* };
        }
        role!(
            surface, surface_container, surface_container_high, surface_container_highest, on_surface, on_surface_variant,
            outline, outline_variant, primary, on_primary, primary_container, on_primary_container, secondary, tertiary,
            error, scrim
        );
        p
    }

    pub fn stamp() -> Option<std::time::SystemTime> {
        std::fs::metadata(colors_file()).and_then(|m| m.modified()).ok()
    }
}

/// `c` with its alpha scaled by `a` (0.0 to 1.0).
pub fn with_alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], (c[3] as f32 * a.clamp(0.0, 1.0)) as u8]
}

/// `c` with its alpha set to `a` (0 to 255).
pub fn alpha(c: Rgba, a: u8) -> Rgba {
    [c[0], c[1], c[2], a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(hex("#8b7bff"), Some([0x8b, 0x7b, 0xff, 255]));
        assert_eq!(hex("8b7bff80"), Some([0x8b, 0x7b, 0xff, 0x80]));
        assert_eq!(hex("nope"), None);
    }

    #[test]
    fn templates_roundtrip_through_the_config_text() {
        let text = "[config]\n\n[templates.kitty]\ninput_path = '~/a b'\noutput_path = \"/x/y\"\npost_hook = 'kitty @ set-colors -a'\n\n[templates.vesper]\ninput_path = 'z'\noutput_path = 'w'\n";
        let all = parse_templates(text);
        assert_eq!(all.len(), 2);
        let kitty = all.iter().find(|t| t.name == "kitty").unwrap();
        assert_eq!((kitty.input_path.as_str(), kitty.output_path.as_str(), kitty.post_hook.as_str()), ("~/a b", "/x/y", "kitty @ set-colors -a"));
    }

    #[test]
    fn builtin_template_has_every_palette_role() {
        for role in [
            "surface", "surface_container", "surface_container_high", "surface_container_highest", "on_surface",
            "on_surface_variant", "outline", "outline_variant", "primary", "on_primary", "primary_container",
            "on_primary_container", "secondary", "tertiary", "error", "scrim",
        ] {
            assert!(BUILTIN_TEMPLATE.contains(&format!("{role} = ")), "missing {role}");
        }
    }

    #[test]
    fn error_text_is_cleaned() {
        let raw = "Error: \n   0: \u{1b}[91mFailed to get source color.\u{1b}[0m\n   1: \u{1b}[91mCould not get source color from image: a.jpg\u{1b}[0m\n\nBacktrace omitted. Run with RUST_BACKTRACE=1 environment variable to display it.\nRun with RUST_BACKTRACE=full to include source snippets.\n";
        assert_eq!(clean_error(raw), "Failed to get source color. - Could not get source color from image: a.jpg");
        assert_eq!(clean_error(""), "matugen failed");
    }

    #[test]
    fn finds_binaries_by_path_and_by_search() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vesper-bin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("vesper-test-tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.join("not-executable");
        std::fs::write(&plain, "x").unwrap();

        assert_eq!(find_binary(&tool.to_string_lossy()), Some(tool.clone()), "absolute path");
        assert_eq!(find_binary(&plain.to_string_lossy()), None, "not executable");
        assert_eq!(find_binary("/no/such/dir/matugen"), None);
        assert!(find_binary("sh").is_some(), "common tools are found in the standard folders");
        assert_eq!(find_binary("definitely-not-installed-vesper-xyz"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alpha_helpers() {
        assert_eq!(with_alpha([1, 2, 3, 200], 0.5), [1, 2, 3, 100]);
        assert_eq!(alpha([1, 2, 3, 200], 9), [1, 2, 3, 9]);
    }
}

// ------------------------------------------------------------------ built-in app templates (kitty, File Manager)

fn app_template_file(name: &str) -> PathBuf {
    config_dir().join("templates").join(name)
}

fn xdg_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config"))
}

fn xdg_state() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".local/state"))
}

/// Where the kitty colours are written; `kitty.conf` includes it.
pub fn kitty_colors_file() -> PathBuf {
    xdg_config().join("kitty/vesper-colors.conf")
}

/// Where the foot colours are written; `foot.ini` includes it.
pub fn foot_colors_file() -> PathBuf {
    xdg_config().join("foot/vesper-colors.ini")
}

/// Where the fetch colours are written; fetch reads it on every start.
pub fn fetch_colors_file() -> PathBuf {
    xdg_config().join("fetch/colors")
}

/// Where the shell prompt colours are written.
pub fn shell_colors_file() -> PathBuf {
    xdg_state().join("vesper/shell-colors.sh")
}

/// File Manager reads every `*.json` in this folder as a theme; this one is called `vesper`.
pub fn file_manager_theme_file() -> PathBuf {
    xdg_state().join("file-manager/themes/vesper.json")
}

/// Where the Vesktop theme goes: the flatpak's folder and a normal install's, each only if that Vesktop has run (has a config
/// folder). Vesktop lists every `*.theme.css` in its `themes` folder.
pub fn vesktop_theme_files() -> Vec<(&'static str, PathBuf)> {
    let mut files = Vec::new();
    let flatpak = home().join(".var/app/dev.vencord.Vesktop/config/vesktop");
    if flatpak.is_dir() {
        files.push(("vesper-vesktop", flatpak.join("themes/vesper.theme.css")));
    }
    let native = xdg_config().join("vesktop");
    if native.is_dir() {
        files.push(("vesper-vesktop-native", native.join("themes/vesper.theme.css")));
    }
    files
}

fn file_manager_settings_file() -> PathBuf {
    xdg_config().join("file-manager/settings.json")
}

/// Make `kitty.conf` include the generated colours (once). Everything else in the file is left alone.
fn link_kitty() {
    let conf = xdg_config().join("kitty/kitty.conf");
    let text = std::fs::read_to_string(&conf).unwrap_or_default();
    if text.contains("vesper-colors.conf") {
        return;
    }
    let mut new = text;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str("\n# Added by Vesper: colours follow the wallpaper (turn off in Settings > Personalization).\ninclude vesper-colors.conf\n");
    let _ = write_atomic(&conf, &new);
}

/// Remove the include again.
fn unlink_kitty() {
    let conf = xdg_config().join("kitty/kitty.conf");
    let Ok(text) = std::fs::read_to_string(&conf) else { return };
    if !text.contains("vesper-colors.conf") {
        return;
    }
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("vesper-colors.conf") && !l.starts_with("# Added by Vesper: colours follow")).collect();
    let _ = write_atomic(&conf, &(kept.join("\n") + "\n"));
}

/// Make `foot.ini` include the generated colours (once). Everything else in the file is left alone.
fn link_foot() {
    let conf = xdg_config().join("foot/foot.ini");
    let text = std::fs::read_to_string(&conf).unwrap_or_default();
    if text.contains("vesper-colors.ini") {
        return;
    }
    let mut new = text;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str("\n# Added by Vesper: colours follow the wallpaper (turn off in Settings > Personalization).\ninclude=~/.config/foot/vesper-colors.ini\n");
    let _ = write_atomic(&conf, &new);
}

/// Remove the include again.
fn unlink_foot() {
    let conf = xdg_config().join("foot/foot.ini");
    let Ok(text) = std::fs::read_to_string(&conf) else { return };
    if !text.contains("vesper-colors.ini") {
        return;
    }
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("vesper-colors.ini") && !l.starts_with("# Added by Vesper: colours follow")).collect();
    let _ = write_atomic(&conf, &(kept.join("\n") + "\n"));
}

/// Select (or deselect) the generated theme in File Manager's settings. Only touches the `theme` key.
fn set_file_manager_theme(on: bool) {
    let path = file_manager_settings_file();
    let mut value: serde_json::Value =
        std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    let Some(obj) = value.as_object_mut() else { return };
    let current = obj.get("theme").and_then(|v| v.as_str()).unwrap_or("system").to_owned();
    if on && current != "custom:vesper" {
        obj.insert("theme".into(), "custom:vesper".into());
    } else if !on && current == "custom:vesper" {
        obj.insert("theme".into(), "system".into());
    } else {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = write_atomic(&path, &text);
    }
}

/// Apply the on/off state of the app templates: link or unlink them in the apps' own settings.
/// Call this after changing `ThemeSettings::kitty` or `file_manager`.
pub fn apply_app_links(settings: &ThemeSettings) {
    if settings.kitty && settings.enabled {
        link_kitty();
    } else {
        unlink_kitty();
    }
    if settings.foot && settings.enabled {
        link_foot();
    } else {
        unlink_foot();
    }
    if !(settings.shell && settings.enabled) {
        let _ = std::fs::remove_file(shell_colors_file());
    }
    if !(settings.fetch && settings.enabled) {
        let _ = std::fs::remove_file(fetch_colors_file());
    }
    set_file_manager_theme(settings.file_manager && settings.enabled);
}

/// Make every running component re-read its colours (they watch the modification time of the colours file): call after
/// changing `[colors]`.
pub fn touch_colors() {
    let path = colors_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

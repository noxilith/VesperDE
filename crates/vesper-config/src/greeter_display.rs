//! Making the login screen use the display scale chosen in Settings.
//!
//! The login screen runs as the `greeter` user and reads `/etc/vesper/config.toml`, not your own configuration (your home
//! folder is not open to it). So the session writes the monitors' scales to `~/.cache/vesper/greeter-display.toml` and asks a
//! small root helper (`/usr/libexec/vesper/sync-greeter-display`, allowed through sudoers for the wheel group, see
//! `packaging/`) to copy them into [`SYSTEM_FILE`], which the login screen reads. The helper keeps only lines of the exact
//! shape [`render`] writes, so the file in your folder cannot put anything else there.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::{Config, OutputConfig};

/// What the login screen reads (written by the root helper).
pub const SYSTEM_FILE: &str = "/etc/vesper/greeter-display.toml";
/// The root helper that copies the user's file there.
pub const HELPER: &str = "/usr/libexec/vesper/sync-greeter-display";

/// Where the session leaves its scales for the helper.
pub fn user_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/vesper/greeter-display.toml"))
}

/// A connector name the helper accepts (it filters the file with the same rule).
fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.@:+-".contains(c))
}

/// The scales set by hand (not "automatic") in the shape the helper keeps.
pub fn render(outputs: &BTreeMap<String, OutputConfig>) -> String {
    let mut out = String::new();
    for (name, o) in outputs {
        if name_ok(name) && (0.5..=4.0).contains(&o.scale) {
            out += &format!("[display.outputs.\"{name}\"]\nscale = {}\n", o.scale);
        }
    }
    out
}

/// The scale of each monitor in `text` (the helper's file).
pub fn parse(text: &str) -> BTreeMap<String, f64> {
    let Ok(table) = text.parse::<toml::Table>() else { return BTreeMap::new() };
    let Some(outputs) = table.get("display").and_then(|d| d.get("outputs")).and_then(|o| o.as_table()) else { return BTreeMap::new() };
    outputs
        .iter()
        .filter_map(|(name, v)| {
            let s = v.get("scale")?;
            let scale = s.as_float().or_else(|| s.as_integer().map(|i| i as f64))?;
            (0.5..=4.0).contains(&scale).then(|| (name.clone(), scale))
        })
        .collect()
}

/// For the login screen: take the monitors' scales from [`SYSTEM_FILE`], keeping everything else as the system config has it.
pub fn apply_to_greeter(config: &mut Config) {
    let Ok(text) = std::fs::read_to_string(SYSTEM_FILE) else { return };
    for (name, scale) in parse(&text) {
        config.display.outputs.entry(name).or_default().scale = scale;
    }
}

/// For the session: if the scales differ from what the login screen has, leave them for the helper and run it. Does nothing,
/// quietly, where the helper or its sudoers rule is not installed.
pub fn publish(config: &Config) {
    let wanted = render(&config.display.outputs);
    let current = std::fs::read_to_string(SYSTEM_FILE).unwrap_or_default();
    if wanted == current || !std::path::Path::new(HELPER).is_file() {
        return;
    }
    let Some(file) = user_file() else { return };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&file, &wanted).is_err() {
        return;
    }
    // `-n`: never ask for a password (there is nobody to type it); without the sudoers rule this simply fails
    if let Ok(mut child) = Command::new("sudo").args(["-n", HELPER]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(list: &[(&str, f64)]) -> BTreeMap<String, OutputConfig> {
        list.iter().map(|(n, s)| (n.to_string(), OutputConfig { scale: *s, ..Default::default() })).collect()
    }

    #[test]
    fn renders_only_scales_set_by_hand() {
        let text = render(&outputs(&[("eDP-1", 1.5), ("HDMI-A-1", 0.0), ("DP-2", 2.0), ("bad\"name", 1.0)]));
        assert_eq!(text, "[display.outputs.\"DP-2\"]\nscale = 2\n[display.outputs.\"eDP-1\"]\nscale = 1.5\n");
    }

    #[test]
    fn parses_what_it_renders() {
        let o = outputs(&[("eDP-1", 1.25), ("DP-2", 2.0)]);
        let back = parse(&render(&o));
        assert_eq!(back.get("eDP-1"), Some(&1.25));
        assert_eq!(back.get("DP-2"), Some(&2.0));
        assert!(parse("not toml [").is_empty());
        assert!(parse("[display.outputs.\"X\"]\nscale = 9\n").is_empty(), "out of range is ignored");
    }
}

//! Which distribution is running, and the logos available for it.

use std::{collections::HashMap, fs, path::PathBuf};

/// `/etc/os-release` (or `/usr/lib/os-release`) as key/value pairs.
pub fn os_release() -> HashMap<String, String> {
    let text = fs::read_to_string("/etc/os-release").or_else(|_| fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_owned(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()))
        })
        .collect()
}

/// Font glyphs (Nerd Font "Font Logos" range) for common distributions: (id, label, glyph).
pub const GLYPHS: [(&str, &str, char); 16] = [
    ("linux", "Linux (Tux)", '\u{f17c}'),
    ("arch", "Arch Linux", '\u{f303}'),
    ("debian", "Debian", '\u{f306}'),
    ("ubuntu", "Ubuntu", '\u{f31b}'),
    ("fedora", "Fedora", '\u{f30a}'),
    ("nixos", "NixOS", '\u{f313}'),
    ("gentoo", "Gentoo", '\u{f30d}'),
    ("opensuse", "openSUSE", '\u{f314}'),
    ("manjaro", "Manjaro", '\u{f312}'),
    ("linuxmint", "Linux Mint", '\u{f30e}'),
    ("void", "Void", '\u{f32e}'),
    ("alpine", "Alpine", '\u{f300}'),
    ("endeavouros", "EndeavourOS", '\u{f322}'),
    ("pop", "Pop!_OS", '\u{f32a}'),
    ("kali", "Kali", '\u{f327}'),
    ("raspbian", "Raspberry Pi OS", '\u{f315}'),
];

/// The glyph for a distribution id (matching `ID` or anything in `ID_LIKE`), or the penguin.
pub fn glyph_for(os: &HashMap<String, String>) -> char {
    let mut ids: Vec<String> = vec![os.get("ID").cloned().unwrap_or_default()];
    ids.extend(os.get("ID_LIKE").map(|s| s.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap_or_default());
    for id in ids {
        let id = if id == "ubuntu-core" { "ubuntu".to_owned() } else { id };
        if let Some((_, _, g)) = GLYPHS.iter().find(|(gid, _, _)| *gid == id || (*gid == "pop" && id == "pop")) {
            return *g;
        }
    }
    GLYPHS[0].2
}

/// Logo files found on this system: (label, absolute path). The one named by `LOGO=` in os-release comes first.
pub fn distro_logos() -> Vec<(String, String)> {
    let os = os_release();
    let want: Vec<String> = ["LOGO", "ID"].iter().filter_map(|k| os.get(*k).cloned()).collect();
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from("/usr/share/pixmaps"), PathBuf::from("/usr/share/icons/hicolor/scalable/apps")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/icons"));
        dirs.push(PathBuf::from(&home).join(".local/share/pixmaps"));
    }
    // theme folders often hold distributor logos
    if let Ok(rd) = fs::read_dir("/usr/share/icons") {
        for e in rd.flatten() {
            dirs.push(e.path().join("places/scalable"));
            dirs.push(e.path().join("scalable/places"));
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for d in dirs {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "svg") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
            let lower = stem.to_lowercase();
            let wanted = want.iter().any(|w| !w.is_empty() && lower == w.to_lowercase());
            if wanted || lower.contains("logo") || lower.starts_with("distributor") {
                let path = p.to_string_lossy().into_owned();
                if !out.iter().any(|(_, o)| *o == path) {
                    out.push((stem, path));
                }
            }
        }
    }
    out.sort_by_key(|(label, _)| !want.iter().any(|w| w.eq_ignore_ascii_case(label)));
    out.truncate(40);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_follow_id_and_id_like() {
        let mut os = HashMap::new();
        os.insert("ID".to_owned(), "cachyos".to_owned());
        os.insert("ID_LIKE".to_owned(), "arch".to_owned());
        assert_eq!(glyph_for(&os), '\u{f303}');
        os.insert("ID".to_owned(), "mystery".to_owned());
        os.remove("ID_LIKE");
        assert_eq!(glyph_for(&os), '\u{f17c}');
    }
}

/// Logos shipped with Vesper: (name, label, svg). The dark variant is white, for dark bars; the light one is near-black.
pub const BUNDLED: [(&str, &str, &[u8]); 2] = [
    ("singularitysys-dark", "SingularitySYS (for dark bars)", include_bytes!("../../../assets/logos/singularitysys-dark.svg")),
    ("singularitysys-light", "SingularitySYS (for light bars)", include_bytes!("../../../assets/logos/singularitysys-light.svg")),
];

/// Is this SingularitySYS (by ID, ID_LIKE or NAME)?
pub fn is_singularity(os: &HashMap<String, String>) -> bool {
    ["ID", "ID_LIKE", "NAME", "PRETTY_NAME"].iter().any(|k| os.get(*k).is_some_and(|v| v.to_lowercase().contains("singularity")))
}

/// Path of a bundled logo, written to the cache folder on first use.
pub fn bundled_path(name: &str) -> Option<PathBuf> {
    let (_, _, bytes) = BUNDLED.iter().find(|(n, _, _)| *n == name)?;
    let base = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("vesper/logos");
    let file = dir.join(format!("{name}.svg"));
    if fs::metadata(&file).map(|m| m.len() as usize).ok() != Some(bytes.len()) {
        fs::create_dir_all(&dir).ok()?;
        fs::write(&file, bytes).ok()?;
    }
    Some(file)
}

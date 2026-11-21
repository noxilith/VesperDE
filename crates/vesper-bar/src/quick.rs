//! The toggles of the quick-settings popup: reading their state and switching them. Everything is done with the usual
//! command line tools (nmcli, bluetoothctl, rfkill, wpctl) or Vesper's own config, so nothing needs a daemon of ours.

use std::process::Command;

use crate::{popup::spawn, sys::Sys};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tile {
    Wifi,
    Bluetooth,
    Airplane,
    NightLight,
    Sound,
    Mic,
    Dnd,
    KeepAwake,
}

/// Every tile with its label, the glyph when it is on and the glyph when it is off.
pub const TILES: [(Tile, &str, &str, &str); 8] = [
    (Tile::Wifi, "Wi-Fi", "\u{f05a9}", "\u{f05aa}"),
    (Tile::Bluetooth, "Bluetooth", "\u{f00af}", "\u{f00b2}"),
    (Tile::Airplane, "Airplane", "\u{f001d}", "\u{f001d}"),
    (Tile::NightLight, "Night", "\u{f0594}", "\u{f0594}"),
    (Tile::Sound, "Sound", "\u{f057e}", "\u{f0581}"),
    (Tile::Mic, "Mic", "\u{f036c}", "\u{f036d}"),
    (Tile::Dnd, "Silent", "\u{f009b}", "\u{f009a}"),
    (Tile::KeepAwake, "Keep awake", "\u{f0176}", "\u{f0176}"),
];

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Quick {
    pub wifi: bool,
    pub bluetooth: bool,
    pub airplane: bool,
    pub night: bool,
    pub sound: bool,
    pub mic: bool,
    pub dnd: bool,
    pub awake: bool,
}

impl Quick {
    pub fn get(&self, t: Tile) -> bool {
        match t {
            Tile::Wifi => self.wifi,
            Tile::Bluetooth => self.bluetooth,
            Tile::Airplane => self.airplane,
            Tile::NightLight => self.night,
            Tile::Sound => self.sound,
            Tile::Mic => self.mic,
            Tile::Dnd => self.dnd,
            Tile::KeepAwake => self.awake,
        }
    }
}

fn out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = crate::cmd::output(Command::new(cmd).args(args)).ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// (type, soft blocked) of every rfkill switch.
fn rfkill() -> Vec<(String, bool)> {
    let Ok(dir) = std::fs::read_dir("/sys/class/rfkill") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| {
            let ty = std::fs::read_to_string(e.path().join("type")).ok()?.trim().to_string();
            let soft = std::fs::read_to_string(e.path().join("soft")).ok()?.trim() == "1";
            Some((ty, soft))
        })
        .collect()
}

/// Is the radio of this type switched on according to rfkill (None when there is no such radio)?
fn radio_on(kind: &str) -> Option<bool> {
    let r: Vec<bool> = rfkill().into_iter().filter(|(t, _)| t == kind).map(|(_, blocked)| !blocked).collect();
    (!r.is_empty()).then(|| r.iter().any(|on| *on))
}

/// Is the Bluetooth daemon running? Without it `bluetoothctl` waits for it on D-Bus until the bar gives up after two
/// seconds, and the bar (which asks on its own thread) freezes for that long every time it looks.
fn bluetoothd_running() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else { return true };
    dir.flatten().any(|e| {
        e.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "bluetoothd")
    })
}

fn muted(source: &str) -> bool {
    out("wpctl", &["get-volume", source]).is_some_and(|s| s.contains("[MUTED]"))
}

pub fn read(sys: &Sys) -> Quick {
    let cfg = vesper_config::Config::load();
    let radios = rfkill();
    Quick {
        wifi: out("nmcli", &["radio", "wifi"]).map(|s| s.trim() == "enabled").or_else(|| radio_on("wlan")).unwrap_or(false),
        bluetooth: bluetoothd_running().then(|| out("bluetoothctl", &["show"])).flatten().map(|s| s.contains("Powered: yes")).or_else(|| radio_on("bluetooth")).unwrap_or(false),
        airplane: !radios.is_empty() && radios.iter().all(|(_, blocked)| *blocked),
        night: cfg.appearance.night_light,
        sound: sys.volume.is_none_or(|v| !v.muted),
        mic: !muted("@DEFAULT_AUDIO_SOURCE@"),
        dnd: cfg.notifications.do_not_disturb,
        awake: vesper_ipc::keep_awake_path().exists(),
    }
}

/// Flip one tile: runs the command and updates `q` to what it should now be.
pub fn toggle(q: &mut Quick, t: Tile) {
    let on = !q.get(t);
    match t {
        Tile::Wifi => {
            q.wifi = on;
            if out("nmcli", &["radio", "wifi", if on { "on" } else { "off" }]).is_none() {
                spawn("rfkill", &[if on { "unblock" } else { "block" }, "wifi"]);
            }
        }
        Tile::Bluetooth => {
            q.bluetooth = on;
            spawn("bluetoothctl", &["power", if on { "on" } else { "off" }]);
        }
        Tile::Airplane => {
            q.airplane = on;
            // airplane mode switches every radio off; leaving it turns them back on
            q.wifi = !on;
            q.bluetooth = !on;
            spawn("rfkill", &[if on { "block" } else { "unblock" }, "all"]);
        }
        Tile::NightLight => {
            q.night = on;
            let _ = vesper_config::Config::set("appearance.night_light", on.into());
        }
        Tile::Sound => {
            q.sound = on;
            spawn("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", if on { "0" } else { "1" }]);
        }
        Tile::Mic => {
            q.mic = on;
            spawn("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", if on { "0" } else { "1" }]);
        }
        Tile::Dnd => {
            q.dnd = on;
            let _ = vesper_config::Config::set("notifications.do_not_disturb", on.into());
        }
        Tile::KeepAwake => {
            q.awake = on;
            let path = vesper_ipc::keep_awake_path();
            if on {
                let _ = std::fs::write(path, "");
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

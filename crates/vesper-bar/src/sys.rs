//! System status for the bar: volume, network, battery. Plain sysfs and `wpctl`, no extra daemons.

use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    pub percent: u32,
    pub muted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Net {
    Wifi,
    Ethernet,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u32,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub cpu: u32,
    pub mem: u32,
    pub temp: Option<u32>,
}

/// Recent CPU and memory use (percent, oldest first) for the start menu graphs, and the memory in KiB.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hist {
    pub cpu: Vec<u32>,
    pub mem: Vec<u32>,
    pub mem_used_kib: u64,
    pub mem_total_kib: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Notif {
    pub unread: u32,
    pub dnd: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sys {
    /// The text of each plugin widget, by id.
    pub plugin: std::collections::HashMap<&'static str, String>,
    /// The default microphone: Some(true) while it is live (not muted), None without one.
    pub mic: Option<bool>,
    /// Active keyboard layout, when more than one is configured.
    pub kb: Option<String>,
    pub media: Option<crate::dbus::Media>,
    pub tray: Vec<crate::dbus::TrayItem>,
    pub stats: Option<Stats>,
    pub brightness: Option<u32>,
    pub notif: Notif,
    pub volume: Option<Volume>,
    pub net: Net,
    pub battery: Option<Battery>,
    /// Removable drives that are plugged in.
    pub drives: Vec<crate::drives::Drive>,
    pub hist: Hist,
}

pub struct Poller {
    pub sys: Sys,
    last_slow: Option<Instant>,
    /// Volume changes arrive as events (`pactl subscribe`); without that we poll it.
    pub volume_events: bool,
    /// Read CPU, memory and temperature each poll (only when the item is shown).
    pub want_stats: bool,
    last_mic: Option<Instant>,
    last_drives: Option<Instant>,
    cpu_prev: Option<(u64, u64)>,
}

impl Poller {
    pub fn new() -> Self {
        Self { sys: Sys { plugin: Default::default(), mic: None, kb: None, media: None, tray: Vec::new(), stats: None, brightness: None, notif: Notif::default(), volume: None, net: Net::Offline, battery: None, drives: Vec::new(), hist: Default::default() }, last_drives: None, last_slow: None, volume_events: false, want_stats: false, last_mic: None, cpu_prev: None }
    }

    /// Refresh everything that is due. Returns true if anything changed. Volume every call (about 1s),
    /// network and battery every 5s.
    pub fn poll(&mut self) -> bool {
        let before = self.sys.clone();
        self.sys.notif = read_notif();
        self.sys.stats = if self.want_stats { self.read_stats() } else { None };
        if let Some(s) = self.sys.stats {
            let h = &mut self.sys.hist;
            h.cpu.push(s.cpu);
            h.mem.push(s.mem);
            if h.cpu.len() > 60 {
                h.cpu.remove(0);
                h.mem.remove(0);
            }
            if let Ok(m) = fs::read_to_string("/proc/meminfo") {
                let get = |k: &str| -> u64 { m.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse().ok()).unwrap_or(0) };
                h.mem_total_kib = get("MemTotal:");
                h.mem_used_kib = h.mem_total_kib.saturating_sub(get("MemAvailable:"));
            }
        }
        if !self.volume_events || self.sys.volume.is_none() {
            self.sys.volume = read_volume();
        }
        // the microphone is cheap to ask and other programs change it (push to talk), so look every couple of seconds
        if self.last_mic.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            self.sys.mic = read_mic();
            self.last_mic = Some(Instant::now());
        }
        if self.last_drives.is_none_or(|t| t.elapsed() >= Duration::from_secs(3)) {
            self.sys.drives = crate::drives::list();
            self.last_drives = Some(Instant::now());
        }
        if self.last_slow.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
            self.sys.net = read_net();
            self.sys.battery = read_battery();
            self.sys.brightness = read_brightness();
            self.last_slow = Some(Instant::now());
        }
        self.sys != before
    }

    fn read_stats(&mut self) -> Option<Stats> {
        let line = fs::read_to_string("/proc/stat").ok()?.lines().next()?.to_string();
        let v: Vec<u64> = line.split_whitespace().skip(1).filter_map(|n| n.parse().ok()).collect();
        if v.len() < 5 {
            return None;
        }
        let total: u64 = v.iter().take(8).sum();
        let idle = v[3] + v[4];
        let cpu = match self.cpu_prev {
            Some((pt, pi)) if total > pt => (100 * ((total - pt) - (idle - pi)) / (total - pt)) as u32,
            _ => 0,
        };
        self.cpu_prev = Some((total, idle));
        Some(Stats { cpu, mem: read_mem().unwrap_or(0), temp: read_temp() })
    }

    pub fn refresh_volume(&mut self) {
        self.sys.volume = read_volume();
        self.sys.mic = read_mic();
    }
}

fn read_mem() -> Option<u32> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    let get = |k: &str| -> Option<u64> { s.lines().find(|l| l.starts_with(k))?.split_whitespace().nth(1)?.parse().ok() };
    let (total, avail) = (get("MemTotal:")?, get("MemAvailable:")?);
    Some((100 * total.saturating_sub(avail) / total.max(1)) as u32)
}

/// The hottest thermal zone in whole degrees.
fn read_temp() -> Option<u32> {
    let mut best: Option<u32> = None;
    for e in fs::read_dir("/sys/class/thermal").ok()?.flatten() {
        if !e.file_name().to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        if let Some(t) = fs::read_to_string(e.path().join("temp")).ok().and_then(|s| s.trim().parse::<i64>().ok()) {
            let c = (t / 1000).clamp(0, 150) as u32;
            best = Some(best.map_or(c, |b| b.max(c)));
        }
    }
    best
}

/// Backlight as a percentage; None on machines without one.
pub fn read_brightness() -> Option<u32> {
    let mut dirs: Vec<_> = fs::read_dir("/sys/class/backlight").ok()?.flatten().collect();
    dirs.sort_by_key(|e| e.file_name());
    let d = dirs.first()?.path();
    let cur: f64 = fs::read_to_string(d.join("brightness")).ok()?.trim().parse().ok()?;
    let max: f64 = fs::read_to_string(d.join("max_brightness")).ok()?.trim().parse().ok()?;
    (max > 0.0).then(|| (100.0 * cur / max).round() as u32)
}

/// The notification daemon publishes "unread dnd" (for example "3 1") in the runtime dir.
fn read_notif() -> Notif {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let s = fs::read_to_string(format!("{dir}/vesper-notify.state")).unwrap_or_default();
    let mut it = s.split_whitespace();
    Notif { unread: it.next().and_then(|n| n.parse().ok()).unwrap_or(0), dnd: it.next() == Some("1") }
}

/// Is the default microphone live? None when there is no microphone.
fn read_mic() -> Option<bool> {
    let out = crate::cmd::output(Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    if !s.contains("Volume:") {
        return None;
    }
    // With no microphone at all (no sound card, or its driver or firmware is missing) PipeWire still answers for
    // @DEFAULT_AUDIO_SOURCE@: it names its dummy output, which never shows as muted. That is not a microphone.
    let info = crate::cmd::output(Command::new("wpctl").args(["inspect", "@DEFAULT_AUDIO_SOURCE@"])).ok()?;
    if !is_source(&String::from_utf8_lossy(&info.stdout)) {
        return None;
    }
    Some(!s.contains("[MUTED]"))
}

/// Does `wpctl inspect` output describe a capture device (`media.class = "Audio/Source"`, also virtual ones)?
fn is_source(inspect: &str) -> bool {
    inspect.lines().any(|l| l.trim_start_matches(['*', ' ']).starts_with("media.class") && l.contains("\"Audio/Source"))
}

fn read_volume() -> Option<Volume> {
    let out = crate::cmd::output(Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SINK@"])).ok()?;
    if !out.status.success() {
        return None;
    }
    parse_volume(&String::from_utf8_lossy(&out.stdout))
}

/// "Volume: 0.70" or "Volume: 0.70 [MUTED]".
pub fn parse_volume(s: &str) -> Option<Volume> {
    let v: f32 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(Volume { percent: (v * 100.0).round() as u32, muted: s.contains("[MUTED]") })
}

fn read_net() -> Net {
    let Ok(dir) = fs::read_dir("/sys/class/net") else { return Net::Offline };
    let (mut wifi, mut eth) = (false, false);
    for e in dir.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name == "lo" {
            continue;
        }
        let up = fs::read_to_string(e.path().join("operstate")).is_ok_and(|s| s.trim() == "up");
        if !up {
            continue;
        }
        if e.path().join("wireless").exists() {
            wifi = true;
        } else if !name.starts_with("sit") && !name.starts_with("veth") && !name.starts_with("docker") && !name.starts_with("br-") {
            eth = true;
        }
    }
    if eth {
        Net::Ethernet
    } else if wifi {
        Net::Wifi
    } else {
        Net::Offline
    }
}

fn read_battery() -> Option<Battery> {
    let mut dirs: Vec<_> = fs::read_dir("/sys/class/power_supply").ok()?.flatten().collect();
    dirs.sort_by_key(|e| e.file_name());
    let bat = dirs.into_iter().find(|e| e.file_name().to_string_lossy().starts_with("BAT"))?;
    let percent: u32 = fs::read_to_string(bat.path().join("capacity")).ok()?.trim().parse().ok()?;
    let status = fs::read_to_string(bat.path().join("status")).unwrap_or_default();
    Some(Battery { percent, charging: matches!(status.trim(), "Charging" | "Full") })
}

/// Run `wpctl` with `args` without waiting on the UI thread.
pub fn wpctl(args: &[&str]) {
    if let Ok(mut child) = Command::new("wpctl").args(args).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_capture_devices_count_as_a_microphone() {
        assert!(is_source("id 52, type PipeWire:Interface:Node\n  * media.class = \"Audio/Source\"\n"));
        assert!(is_source("    media.class = \"Audio/Source/Virtual\""));
        assert!(!is_source("id 37, type PipeWire:Interface:Node\n    media.class = \"Audio/Sink\"\n"));
        assert!(!is_source(""));
    }

    #[test]
    fn volume_parsing() {
        assert_eq!(parse_volume("Volume: 0.70\n"), Some(Volume { percent: 70, muted: false }));
        assert_eq!(parse_volume("Volume: 0.35 [MUTED]"), Some(Volume { percent: 35, muted: true }));
        assert_eq!(parse_volume("garbage"), None);
    }
}

/// Call `on_change` whenever the sound server changes (volume, mute, devices), as soon as it happens.
/// Uses `pw-dump -m` (PipeWire's own change stream, which works without a pulse server), else `pactl subscribe`.
/// Returns false if neither could be started (then the caller polls instead).
pub fn watch_volume(on_change: impl Fn() + Send + 'static) -> bool {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut on_change = Some(on_change);
    // PipeWire: `pw-dump -m` prints the whole graph once, then one JSON array per change; a line holding just `]` ends an array
    if let Ok(mut child) = Command::new("pw-dump").arg("-m").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        std::thread::sleep(Duration::from_millis(300));
        if matches!(child.try_wait(), Ok(None)) {
            if let Some(out) = child.stdout.take() {
                let cb = on_change.take().unwrap();
                std::thread::spawn(move || {
                    let mut batches = 0u32;
                    let mut last = Instant::now() - Duration::from_secs(1);
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if line != "]" {
                            continue;
                        }
                        batches += 1;
                        if batches == 1 {
                            continue; // the first array is the initial picture, not a change
                        }
                        // a burst of changes: look at most 20 times a second, and always at the last state
                        let gap = Duration::from_millis(50);
                        if last.elapsed() < gap {
                            std::thread::sleep(gap - last.elapsed());
                        }
                        last = Instant::now();
                        cb();
                    }
                    let _ = child.wait();
                });
                return true;
            }
        }
    }
    let on_change = on_change.take().unwrap();
    let Ok(mut child) = Command::new("pactl").arg("subscribe").stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    let Some(out) = child.stdout.take() else { return false };
    // without a working pulse server `pactl subscribe` exits at once: then there are no events and the caller must poll
    std::thread::sleep(Duration::from_millis(300));
    if !matches!(child.try_wait(), Ok(None)) {
        return false;
    }
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if line.contains(" sink ") || line.contains(" source ") || line.contains("server") {
                on_change();
            }
        }
        let _ = child.wait();
    });
    true
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use std::sync::{atomic::{AtomicU32, Ordering}, Arc};

    /// Needs a running PipeWire and flips the microphone mute briefly: `cargo test -p vesper-bar -- --ignored watch`.
    #[test]
    #[ignore]
    fn sees_a_mute_change_quickly() {
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        assert!(watch_volume(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        let was = read_mic();
        std::thread::sleep(Duration::from_millis(500));
        let before = hits.load(Ordering::SeqCst);
        let t = Instant::now();
        let _ = Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", if was == Some(true) { "1" } else { "0" }]).status();
        let mut seen = None;
        while t.elapsed() < Duration::from_secs(2) {
            if hits.load(Ordering::SeqCst) > before {
                seen = Some(t.elapsed());
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // put it back
        let _ = Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", if was == Some(true) { "0" } else { "1" }]).status();
        println!("change seen after {seen:?}");
        assert!(seen.is_some_and(|d| d < Duration::from_millis(600)));
    }
}

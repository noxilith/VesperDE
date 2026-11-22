//! Popups that open from the bar: calendar, volume, battery and wifi. They are drawn into a transparent overlay
//! surface that starts below the bar; clicking anywhere on it outside the box closes the popup.

use std::process::Command;

use chrono::{Datelike, Local, NaiveDate};
use vesper_config::theme::with_alpha;
use vesper_gfx::{Canvas, Rgba};

use crate::draw::Renderer;
use crate::shape::{self, Shape};
use crate::sys::Sys;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Brightness,
    Start,
    Calendar,
    Volume,
    Battery,
    Network,
    Drives,
    /// The menu of a tray icon (right-click).
    TrayMenu,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slider {
    Volume,
    Brightness,
    /// Night-light strength in the side panel.
    Night,
    /// One app's own volume in the mixer (index into Popup.apps).
    App(usize),
    /// The microphone level in the microphone panel.
    Mic,
}

#[derive(Clone, Copy, PartialEq, Debug)]
#[allow(dead_code)]
pub enum StartAct {
    PowerMenu,
    Settings,
    Clipboard,
    Emoji,
    Wallpapers,
    Screenshot,
    CopyText,
    ReadQr,
    Record,
    Apps,
    Files,
    Lock,
    Sleep,
    Logout,
    Restart,
    Shutdown,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PHit {
    Start(StartAct),
    /// A slider; the rect tells the position.
    Slider(Slider),
    Tile(crate::quick::Tile),
    /// Open the side panel of a switch.
    Open(Side),
    Bt(usize),
    BtPower,
    BtScan,
    WifiConnect,
    WifiCancel,
    Source(usize),
    /// The push-to-talk switch in the microphone panel.
    Ptt,
    /// "Set push-to-talk key": the next key you press becomes it.
    PttLearn,
    /// The volume boost switch in the volume panel.
    Boost,
    Mute,
    Sink(usize),
    /// A removable drive: mount and open it.
    Drive(usize),
    /// Eject a removable drive.
    Eject(usize),
    Month(i32),
    Profile(usize),
    Net(usize),
    /// The refresh button of the Wi-Fi panel.
    WifiRefresh,
    /// A line of the tray menu that is showing (index into the level that is open).
    MenuItem(usize),
    /// Back from a submenu of the tray menu.
    MenuBack,
}

pub type Rect = (i32, i32, i32, i32);

#[derive(Clone, Debug)]
pub struct Sink {
    pub name: String,
    pub label: String,
    pub default: bool,
}

#[derive(Clone, Debug)]
pub struct Wifi {
    pub ssid: String,
    pub signal: u32,
    pub secure: bool,
    pub active: bool,
}

/// A panel that opens beside the start menu from one of its switches.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    /// Beside the volume slider: mute and the output devices.
    Volume,
    /// Beside the brightness slider: night light.
    Brightness,
    Wifi,
    Bluetooth,
    Sound,
    Mic,
    Night,
}

/// Which switch has a side panel.
pub fn side_for(t: crate::quick::Tile) -> Option<Side> {
    use crate::quick::Tile;
    match t {
        Tile::Wifi => Some(Side::Wifi),
        Tile::Bluetooth => Some(Side::Bluetooth),
        Tile::Sound => Some(Side::Sound),
        Tile::Mic => Some(Side::Mic),
        Tile::NightLight => Some(Side::Night),
        _ => None,
    }
}

/// The default microphone's level in percent.
fn read_mic_percent() -> u32 {
    run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"]).and_then(|s| crate::sys::parse_volume(&s)).map_or(0, |v| v.percent)
}

fn read_sources() -> Vec<Sink> {
    wpctl_nodes("Sources")
}

/// Asking for a Wi-Fi password inside a popup.
#[derive(Clone, Debug, Default)]
pub struct Prompt {
    pub ssid: String,
    pub text: String,
    pub error: Option<String>,
    /// The connection attempt is running.
    pub busy: bool,
}

/// Height of the password prompt (title, field, message line, buttons).
pub const PROMPT_H: i32 = 24 + 34 + 26 + 32;

pub struct Popup {
    pub kind: Kind,
    /// Horizontal centre of the bar item that opened it.
    pub anchor: i32,
    pub month_offset: i32,
    pub sinks: Vec<Sink>,
    /// Apps playing sound, with their own volume (the mixer in the volume panel).
    pub apps: Vec<AppVol>,
    pub profiles: Vec<String>,
    pub profile: String,
    pub nets: Vec<Wifi>,
    pub nets_error: Option<String>,
    /// Label and value rows of the battery popup (status, time left, health, power draw, size, cycles).
    pub battery_rows: Vec<(String, String)>,
    /// The output volume and whether it is muted, and the microphone level (set when the popup is drawn or a panel opens).
    pub vol: (u32, bool),
    pub mic_percent: u32,
    pub bright: Option<u32>,
    pub hits: Vec<(Rect, PHit)>,
    pub rect: Rect,
    /// Restart or shut down waits for a second click.
    pub pending: Option<StartAct>,
    /// State of the quick-settings toggles when the popup was opened.
    pub quick: crate::quick::Quick,
    /// The side panel that is open, where it was drawn, and its data.
    pub side: Option<Side>,
    pub side_rect: Rect,
    pub sources: Vec<Sink>,
    pub night_strength: u32,
    pub ptt: bool,
    pub ptt_key: String,
    /// The Wi-Fi password prompt, when a secured network that is not saved was picked.
    pub prompt: Option<Prompt>,
    /// The tray icon menu being shown, and the way into it (which submenus are open, as indexes).
    pub menu: Option<crate::dbus::TrayMenu>,
    pub menu_path: Vec<usize>,
}

impl Popup {
    pub fn open(kind: Kind, anchor: i32, sys: &Sys) -> Self {
        let mut p = Popup {
            kind,
            anchor,
            month_offset: 0,
            sinks: Vec::new(),
            profiles: Vec::new(),
            profile: String::new(),
            nets: Vec::new(),
            nets_error: None,
            battery_rows: Vec::new(),
            vol: (0, false),
            mic_percent: 0,
            bright: None,
            hits: Vec::new(),
            rect: (0, 0, 0, 0),
            pending: None,
            quick: Default::default(),
            side: None,
            side_rect: (0, 0, 0, 0),
            apps: Vec::new(),
            sources: Vec::new(),
            night_strength: 35,
            ptt: false,
            ptt_key: String::new(),
            prompt: None,
            menu: None,
            menu_path: Vec::new(),
        };
        match kind {
            Kind::Volume => {
                p.sinks = read_sinks();
                p.apps = read_apps();
                p.quick = crate::quick::read(sys);
            }
            Kind::Battery => {
                p.battery_rows = battery_rows();
                (p.profiles, p.profile) = read_profiles();
            }
            Kind::Network => {
                p.quick = crate::quick::read(sys);
                match read_wifi() {
                    Ok(n) => p.nets = n,
                    Err(e) => p.nets_error = Some(e),
                }
            }
            Kind::Start => {
                p.quick = crate::quick::read(sys);
                p.battery_rows = battery_rows();
                (p.profiles, p.profile) = read_profiles();
            }
            Kind::Brightness => {
                p.quick = crate::quick::read(sys);
                p.night_strength = vesper_config::Config::load().appearance.night_light_strength.clamp(1, 100);
            }
            Kind::Calendar | Kind::Drives | Kind::TrayMenu => {}
        }
        p
    }

    pub fn at(&self, x: f64, y: f64) -> Option<PHit> {
        let (x, y) = (x as i32, y as i32);
        self.hits.iter().find(|((x0, y0, x1, y1), _)| x >= *x0 && x < *x1 && y >= *y0 && y < *y1).map(|(_, h)| *h)
    }

    pub fn inside(&self, x: f64, y: f64) -> bool {
        let (x, y) = (x as i32, y as i32);
        let within = |(rx, ry, rw, rh): Rect| x >= rx && x < rx + rw && y >= ry && y < ry + rh;
        within(self.rect) || (self.side.is_some() && within(self.side_rect))
    }

    /// Open the side panel `s` (or close it if it is the one showing) and load what it lists.
    pub fn toggle_side(&mut self, s: Side) {
        // the Bluetooth panel searches while it is open
        if self.side == Some(Side::Bluetooth) {
            crate::bt::send(crate::bt::Cmd::Scan(false));
        }
        if self.side == Some(s) {
            self.side = None;
            return;
        }
        self.side = Some(s);
        if s == Side::Wifi {
            // look for networks again in the background: what is listed now is what was found so far, and the refresh button reads the list again
            spawn("nmcli", &["device", "wifi", "rescan"]);
        }
        match s {
            Side::Wifi => match read_wifi() {
                Ok(n) => {
                    self.nets = n;
                    self.nets_error = None;
                }
                Err(e) => {
                    self.nets.clear();
                    self.nets_error = Some(e);
                }
            },
            Side::Bluetooth => crate::bt::send(crate::bt::Cmd::Scan(true)),
            Side::Sound => {
                self.sinks = read_sinks();
                self.apps = read_apps();
            }
            Side::Mic => {
                self.sources = read_sources();
                self.mic_percent = read_mic_percent();
                let media = vesper_config::Config::load().media;
                self.ptt = media.ptt;
                self.ptt_key = media.ptt_key;
            }
            Side::Volume => {
                self.sinks = read_sinks();
                self.apps = read_apps();
            }
            Side::Brightness | Side::Night => self.night_strength = vesper_config::Config::load().appearance.night_light_strength.clamp(1, 100),
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = crate::cmd::output(Command::new(cmd).args(args)).ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command without waiting for it on the UI thread.
/// A screen recording is running (see scripts/vesper-record).
#[allow(dead_code)]
pub fn recording() -> bool {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::fs::read_to_string(format!("{dir}/vesper-record.pid")).ok().and_then(|p| p.trim().parse::<u32>().ok()).is_some_and(|p| std::path::Path::new(&format!("/proc/{p}")).exists())
}

/// The recorder script: on PATH once installed, else the one in this checkout (next to target/).
pub fn recorder() -> String {
    let dev = std::env::current_exe().ok().and_then(|e| e.ancestors().nth(3).map(|r| r.join("scripts/vesper-record"))).filter(|p| p.exists());
    dev.map_or_else(|| "vesper-record".into(), |p| p.to_string_lossy().into_owned())
}

pub fn spawn(cmd: &str, args: &[&str]) {
    if let Ok(mut child) = Command::new(cmd).args(args).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// The audio devices in the `section` ("Sinks" or "Sources") of `wpctl status`: the id goes in `name` (for
/// `wpctl set-default`), the description in `label`, and the one marked with `*` is the default.
fn wpctl_nodes(section: &str) -> Vec<Sink> {
    let Some(text) = run("wpctl", &["status"]) else { return Vec::new() };
    parse_wpctl_nodes(&text, section)
}

fn parse_wpctl_nodes(text: &str, section: &str) -> Vec<Sink> {
    let mut out = Vec::new();
    let (mut in_audio, mut in_section) = (false, false);
    for raw in text.lines() {
        // the tree characters in front of each line are not part of the text
        let line = raw.trim_start_matches(|c: char| c == ' ' || "│├└─".contains(c));
        if !raw.starts_with(' ') && !raw.starts_with('│') && !raw.is_empty() {
            in_audio = raw.starts_with("Audio");
            in_section = false;
            continue;
        }
        if !in_audio {
            continue;
        }
        if line.ends_with(':') {
            in_section = line.trim_end_matches(':') == section;
            continue;
        }
        if in_section {
            let default = line.starts_with('*');
            let rest = line.trim_start_matches('*').trim_start();
            if let Some((id, tail)) = rest.split_once(". ") {
                if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
                    let label = tail.split(" [").next().unwrap_or(tail).trim().to_string();
                    out.push(Sink { name: id.to_string(), label, default });
                }
            }
        }
    }
    out
}

fn read_sinks() -> Vec<Sink> {
    wpctl_nodes("Sinks")
}

/// One app that is playing sound: the wpctl id, its name and its own volume.
#[derive(Clone, Debug, PartialEq)]
pub struct AppVol {
    pub id: String,
    pub label: String,
    pub percent: u32,
    pub muted: bool,
}

/// The playback streams in `wpctl status` (the apps), each with its volume from `wpctl get-volume`.
fn read_apps() -> Vec<AppVol> {
    let Some(text) = run("wpctl", &["status"]) else { return Vec::new() };
    parse_wpctl_streams(&text)
        .into_iter()
        .map(|(id, label)| {
            let v = run("wpctl", &["get-volume", &id]).unwrap_or_default();
            let frac = v.split_whitespace().nth(1).and_then(|n| n.parse::<f32>().ok()).unwrap_or(1.0);
            AppVol { id, label, percent: (frac * 100.0).round() as u32, muted: v.contains("[MUTED]") }
        })
        .collect()
}

/// The app streams of the Audio part of `wpctl status`: (id, name). Port lines under a stream (with an arrow or a state)
/// are not streams.
fn parse_wpctl_streams(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let (mut in_audio, mut in_section) = (false, false);
    for raw in text.lines() {
        let line = raw.trim_start_matches(|c: char| c == ' ' || "│├└─".contains(c));
        if !raw.starts_with(' ') && !raw.starts_with('│') && !raw.is_empty() {
            in_audio = raw.starts_with("Audio");
            in_section = false;
            continue;
        }
        if !in_audio {
            continue;
        }
        if line.ends_with(':') {
            in_section = line.trim_end_matches(':') == "Streams";
            continue;
        }
        if in_section && !line.contains('>') && !line.contains('<') && !line.contains('[') {
            if let Some((id, tail)) = line.split_once(". ") {
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && !tail.trim().is_empty() {
                    out.push((id.to_string(), tail.trim().to_string()));
                }
            }
        }
    }
    out
}


/// The system service that switches power profiles: UPower's name, or the older one (the same interface under another name).
const PROFILE_SERVICES: [(&str, &str); 2] = [
    ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
    ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
];

/// The power profiles on offer (`power-saver`, `balanced`, `performance`) and the one in use. Asks power-profiles-daemon on
/// the system bus directly: `powerprofilesctl` is a Python script, and without Python (some systems have none) it cannot run.
fn read_profiles() -> (Vec<String>, String) {
    use zbus::zvariant::OwnedValue;
    if let Ok(conn) = zbus::blocking::Connection::system() {
        for (name, path) in PROFILE_SERVICES {
            let Ok(p) = zbus::blocking::Proxy::new(&conn, name, path, name) else { continue };
            let Ok(active) = p.get_property::<String>("ActiveProfile") else { continue };
            let list = p.get_property::<Vec<std::collections::HashMap<String, OwnedValue>>>("Profiles").unwrap_or_default();
            let mut names: Vec<String> = list.into_iter().filter_map(|m| m.get("Profile").and_then(|v| String::try_from(v.clone()).ok())).collect();
            names.retain(|n| ["power-saver", "balanced", "performance"].contains(&n.as_str()));
            if !names.is_empty() {
                return (names, active);
            }
        }
    }
    let Some(cur) = run("powerprofilesctl", &["get"]) else { return (Vec::new(), String::new()) };
    let list = run("powerprofilesctl", &["list"]).unwrap_or_default();
    let mut names: Vec<String> = list
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches('*').trim();
            l.strip_suffix(':').filter(|n| !n.contains(' ')).map(str::to_string)
        })
        .collect();
    names.retain(|n| ["power-saver", "balanced", "performance"].contains(&n.as_str()));
    (names, cur.trim().to_string())
}

/// Switch to power profile `name`, off the UI thread.
pub fn set_profile(name: &str) {
    let name = name.to_owned();
    std::thread::spawn(move || {
        if let Ok(conn) = zbus::blocking::Connection::system() {
            for (service, path) in PROFILE_SERVICES {
                let Ok(p) = zbus::blocking::Proxy::new(&conn, service, path, service) else { continue };
                if p.set_property("ActiveProfile", name.clone()).is_ok() {
                    return;
                }
            }
        }
        // the old way, where it can run
        let _ = Command::new("powerprofilesctl").args(["set", &name]).output();
    });
}


/// The details shown in the battery popup, read from sysfs: only the rows the battery can answer.
fn battery_rows() -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return Vec::new() };
    let mut bats: Vec<_> = dir.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("BAT")).collect();
    bats.sort_by_key(|e| e.file_name());
    let Some(bat) = bats.into_iter().next() else { return Vec::new() };
    let name = bat.file_name().to_string_lossy().into_owned();
    let rd = |n: &str| std::fs::read_to_string(bat.path().join(n)).ok().and_then(|s| s.trim().parse::<f64>().ok());
    let status = std::fs::read_to_string(bat.path().join("status")).unwrap_or_default();
    let status = status.trim().to_string();
    let volts = rd("voltage_now").or_else(|| rd("voltage_min_design")).unwrap_or(0.0); // µV
    // energy in µWh, from energy_* or from charge_* (µAh) times the voltage
    let energy = |e: &str, c: &str| rd(e).or_else(|| rd(c).map(|v| v * volts / 1e6));
    let (now, full, design) = (energy("energy_now", "charge_now"), energy("energy_full", "charge_full"), energy("energy_full_design", "charge_full_design"));
    // power in µW, from power_now or from current_now (µA) times the voltage
    let watts = rd("power_now").or_else(|| rd("current_now").map(|a| a * volts / 1e6)).filter(|w| *w > 0.0);
    let mut rows = Vec::new();
    if !status.is_empty() {
        rows.push(("Status".to_string(), status.clone()));
    }
    if let (Some(rate), Some(now), Some(full)) = (watts, now, full) {
        let hours = match status.as_str() {
            "Discharging" => Some(("Time left", now / rate)),
            "Charging" => Some(("Until full", (full - now).max(0.0) / rate)),
            _ => None,
        };
        if let Some((label, h)) = hours {
            let mins = (h * 60.0).round() as u32;
            rows.push((label.to_string(), if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins}m") }));
        }
    }
    if let (Some(full), Some(design)) = (full, design.filter(|d| *d > 0.0)) {
        rows.push((format!("Health ({name})"), format!("{}%", (full / design * 100.0).round() as u32)));
    }
    if let Some(w) = watts {
        rows.push(("Power draw".to_string(), format!("{:.1} W", w / 1e6)));
    }
    if let Some(d) = design {
        rows.push(("Battery size".to_string(), format!("{} Wh", (d / 1e6).round() as u32)));
    }
    if let Some(n) = rd("cycle_count").filter(|n| *n > 0.0) {
        rows.push(("Charge cycles".to_string(), format!("{}", n as u32)));
    }
    rows
}

fn read_wifi() -> Result<Vec<Wifi>, String> {
    let out = crate::cmd::output(Command::new("nmcli").args(["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "no"]))
        .map_err(|_| "nmcli (NetworkManager) is not installed".to_string())?;
    if !out.status.success() {
        return Err("NetworkManager is not running".into());
    }
    let mut nets: Vec<Wifi> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.replace("\\:", "\u{1}");
        let f: Vec<String> = line.split(':').map(|s| s.replace('\u{1}', ":")).collect();
        if f.len() < 4 || f[1].is_empty() {
            continue;
        }
        let n = Wifi { active: f[0] == "*", ssid: f[1].clone(), signal: f[2].parse().unwrap_or(0), secure: !f[3].is_empty() && f[3] != "--" };
        match nets.iter_mut().find(|e| e.ssid == n.ssid) {
            Some(e) => {
                if n.active || (!e.active && n.signal > e.signal) {
                    *e = n;
                }
            }
            None => nets.push(n),
        }
    }
    nets.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    nets.truncate(16);
    if nets.is_empty() {
        if let Some(note) = unmanaged_wifi_note() {
            return Err(note);
        }
    }
    Ok(nets)
}

/// NetworkManager lists nothing for a Wi-Fi adapter it does not manage (the connection is set up by something else, such as
/// a bare wpa_supplicant and dhcpcd). Say so, instead of "No networks found" while the Wi-Fi is working. Only reads.
fn unmanaged_wifi_note() -> Option<String> {
    let out = crate::cmd::output(Command::new("nmcli").args(["-t", "-f", "DEVICE,TYPE,STATE", "device"])).ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let device = text.lines().filter_map(|l| {
        let mut f = l.split(':');
        let (dev, ty, state) = (f.next()?, f.next()?, f.next()?);
        (ty == "wifi" && state == "unmanaged").then(|| dev.to_owned())
    }).next()?;
    let up = std::fs::read_to_string(format!("/sys/class/net/{device}/operstate")).is_ok_and(|s| s.trim() == "up");
    Some(if up { "Connected, but not managed by NetworkManager" } else { "Wi-Fi is not managed by NetworkManager" }.to_string())
}

fn month_start(offset: i32) -> NaiveDate {
    let today = Local::now().date_naive();
    let total = today.year() * 12 + today.month0() as i32 + offset;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap_or(today)
}

impl Renderer {
    /// The brightness and volume sliders and the toggle tiles of the quick settings. Returns the height used.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn quick_block(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, q: crate::quick::Quick, sys: &Sys, hover: Option<PHit>, open: Option<Side>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        let mut cy = y;
        let lit = |s: Side| open == Some(s) || hover == Some(PHit::Open(s));
        cy += self.slider_row_with(c, ix, iw, cy, "Brightness", sys.brightness.unwrap_or(0), sys.brightness.is_none(), Slider::Brightness, Some((Side::Brightness, lit(Side::Brightness))), hits);
        let (percent, mutedv) = sys.volume.map_or((0, false), |v| (v.percent, v.muted));
        cy += self.slider_row_with(c, ix, iw, cy, "Volume", percent, mutedv, Slider::Volume, Some((Side::Volume, lit(Side::Volume))), hits);
        cy += 8;
        let (cols, gap, th) = (4, 8, 68);
        let tw = (iw - (cols - 1) * gap) / cols;
        let big = self.text.resized(21.0);
        let chev = self.text.resized(13.0);
        let small = self.text.resized(11.0);
        for (i, (tile, label, on_glyph, off_glyph)) in crate::quick::TILES.iter().enumerate() {
            let (col, row) = (i as i32 % cols, i as i32 / cols);
            let (tx, ty) = (ix + col * (tw + gap), cy + row * (th + gap));
            let r = (tx, ty, tx + tw, ty + th);
            let on = q.get(*tile);
            let hot = hover == Some(PHit::Tile(*tile));
            let fill = if on { accent } else if hot { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
            c.rounded_rect(r.0, r.1, tw, th, 14.0, fill);
            let fg = if on { self.pal.on_primary } else { self.pal.on_surface };
            big.draw_centered(c, tx + tw / 2, ty + 24, if on { on_glyph } else { off_glyph }, if on { fg } else { accent });
            small.draw_centered(c, tx + tw / 2, ty + 51, &small.fit(label, tw - 8), fg);
            // switches with a side panel get a small arrow in the corner
            if let Some(side) = side_for(*tile) {
                let cr = (tx + tw - 28, ty, tx + tw, ty + 28);
                if open == Some(side) || hover == Some(PHit::Open(side)) {
                    c.rounded_rect(cr.0 + 4, cr.1 + 4, 20, 20, 10.0, with_alpha(fg, 0.25));
                }
                chev.draw_centered(c, cr.0 + 14, cr.1 + 14, "\u{f0142}", fg);
                hits.push((cr, PHit::Open(side)));
            }
            hits.push((r, PHit::Tile(*tile)));
        }
        cy += 2 * th + gap;
        cy - y
    }

    /// The Bluetooth panel: the adapter switch, searching, your devices and the ones nearby.
    fn draw_bt_side(&self, c: &mut Canvas, p: &mut Popup, screen_w: i32, hover: Option<PHit>) {
        let st = crate::bt::snapshot();
        let accent = self.pal.primary;
        let muted = with_alpha(self.pal.on_surface, 0.6);
        let pw = 330;
        let mine: Vec<usize> = (0..st.devices.len()).filter(|&i| st.devices[i].paired).take(6).collect();
        let nearby: Vec<usize> = (0..st.devices.len()).filter(|&i| !st.devices[i].paired).take(6).collect();
        // the height: the switch, then what applies
        let mut ph = 14 + 34 + 10;
        if !st.available {
            ph += 30;
        } else if st.powered {
            ph += 34 + 22; // the search row and the hint at the bottom
            if !mine.is_empty() {
                ph += 24 + 34 * mine.len() as i32;
            }
            if !nearby.is_empty() || st.scanning {
                ph += 24 + 34 * nearby.len().max(1) as i32;
            }
        }
        let (mx, my, mw, _) = p.rect;
        let x = if mx + mw + 8 + pw <= screen_w - 8 { mx + mw + 8 } else { (mx - 8 - pw).max(8) };
        p.side_rect = (x, my, pw, ph);
        self.panel(c, x, my, pw, ph);
        let (ix, iw) = (x + 14, pw - 28);
        let mut cy = my + 14;
        let row = |c: &mut Canvas, hits: &mut Vec<(Rect, PHit)>, y: i32, active: bool, hot: bool, left: &str, right: &str, hit: PHit| {
            let r = (ix, y, ix + iw, y + 30);
            let fill = if active { accent } else if hot { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
            c.rounded_rect(r.0, r.1, iw, 30, 9.0, fill);
            let fg = if active { self.pal.on_primary } else { self.pal.on_surface };
            let rw = if right.is_empty() { 0 } else { self.text.width(right) + 10 };
            self.text.draw(c, ix + 10, y + 15, &self.text.fit(left, iw - 20 - rw), fg);
            if !right.is_empty() {
                self.text.draw(c, ix + iw - rw, y + 15, right, fg);
            }
            hits.push((r, hit));
        };
        if !st.available {
            row(c, &mut p.hits, cy, false, false, "Bluetooth", "", PHit::BtPower);
            self.text.draw(c, ix, cy + 50, &self.text.fit("No Bluetooth adapter (is bluetooth.service running?)", iw), muted);
            return;
        }
        row(c, &mut p.hits, cy, st.powered, hover == Some(PHit::BtPower), "Bluetooth", if st.powered { "on" } else { "off" }, PHit::BtPower);
        cy += 34;
        if !st.powered {
            return;
        }
        let searching = if st.scanning { "Searching for devices\u{2026}" } else { "Search for devices" };
        row(c, &mut p.hits, cy, st.scanning, hover == Some(PHit::BtScan), searching, if st.scanning { "stop" } else { "" }, PHit::BtScan);
        cy += 34;
        let device_row = |c: &mut Canvas, hits: &mut Vec<(Rect, PHit)>, y: i32, i: usize| {
            let d = &st.devices[i];
            let right = match (&d.busy, d.connected, d.rssi) {
                (Some(b), _, _) => b.clone(),
                (None, true, _) => "connected".to_string(),
                (None, false, Some(r)) if !d.paired => (if r >= -60 { "\u{25cf}\u{25cf}\u{25cf}" } else if r >= -75 { "\u{25cf}\u{25cf}\u{25cb}" } else { "\u{25cf}\u{25cb}\u{25cb}" }).to_string(),
                _ => String::new(),
            };
            row(c, hits, y, d.connected, hover == Some(PHit::Bt(i)), &d.name, &right, PHit::Bt(i));
        };
        if !mine.is_empty() {
            self.text.draw(c, ix, cy + 10, "My devices", muted);
            cy += 24;
            for &i in &mine {
                device_row(c, &mut p.hits, cy, i);
                cy += 34;
            }
        }
        if !nearby.is_empty() || st.scanning {
            self.text.draw(c, ix, cy + 10, "Nearby", muted);
            cy += 24;
            for &i in &nearby {
                device_row(c, &mut p.hits, cy, i);
                cy += 34;
            }
            if nearby.is_empty() {
                self.text.draw(c, ix, cy + 12, "Nothing found yet", muted);
            }
        }
        let small = self.text.resized(11.0);
        small.draw(c, ix, my + ph - 16, "Click to connect \u{b7} right-click to forget", muted);
    }

    /// The panel beside the start menu for the switch that was opened (Wi-Fi networks, Bluetooth devices, sound outputs,
    /// microphones, night-light strength).
    fn draw_side(&self, c: &mut Canvas, p: &mut Popup, side: Side, screen_w: i32, hover: Option<PHit>) {
        if side == Side::Bluetooth {
            self.draw_bt_side(c, p, screen_w, hover);
            return;
        }
        let pw = if matches!(side, Side::Sound | Side::Volume | Side::Mic | Side::Wifi | Side::Night | Side::Brightness) { 340 } else { 300 };
        let ph = self.side_height(p, side);
        let (mx, my, mw, _) = p.rect;
        let x = if mx + mw + 8 + pw <= screen_w - 8 { mx + mw + 8 } else { (mx - 8 - pw).max(8) };
        p.side_rect = (x, my, pw, ph);
        self.panel(c, x, my, pw, ph);
        self.draw_side_body(c, p, side, x, my, pw, hover, true);
    }

    fn side_title(side: Side) -> &'static str {
        match side {
            Side::Wifi => "Wi-Fi",
            Side::Bluetooth => "Bluetooth",
            Side::Sound => "Sound output",
            Side::Mic => "Microphone",
            Side::Night => "Night light",
            Side::Brightness => "Brightness",
            Side::Volume => "Volume",
        }
    }

    /// Height of the side panel `side`: title, rows and margins.
    fn side_height(&self, p: &Popup, side: Side) -> i32 {
        if side == Side::Wifi {
            return if p.prompt.is_some() { 14 + 40 + PROMPT_H + 10 } else { 14 + 40 + 24 + p.nets.len().clamp(1, 16) as i32 * 40 + 10 };
        }
        if matches!(side, Side::Sound | Side::Volume) {
            let apps = if p.apps.is_empty() { 28 } else { p.apps.len().min(6) as i32 * 46 };
            return 14 + 40 + 40 + 4 + 62 + 2 + 24 + if p.sinks.is_empty() { 30 } else { p.sinks.len().min(6) as i32 * 38 } + 6 + 24 + apps + 10;
        }
        if side == Side::Mic {
            return 14 + 40 + 40 + 4 + 62 + 2 + 38 + 24 + if p.sources.is_empty() { 30 } else { p.sources.len().min(6) as i32 * 38 } + 10;
        }
        let rows = match side {
            Side::Wifi => p.nets.len().max(1),
            Side::Bluetooth | Side::Night | Side::Brightness => 0,
            Side::Sound => p.sinks.len().clamp(1, 6),
            Side::Mic => p.sources.len().clamp(1, 6) + 2, // push to talk and its key
            Side::Volume => p.sinks.len().clamp(1, 6) + 2, // mute and boost
        };
        let mixer_h = if side == Side::Volume { 30 + if p.apps.is_empty() { 30 } else { p.apps.len().min(6) as i32 * 46 } } else { 0 };
        if side == Side::Wifi && p.prompt.is_some() {
            14 + 32 + PROMPT_H + 10
        } else if matches!(side, Side::Night | Side::Brightness) {
            14 + 40 + 40 + 4 + 62 + 2 + 40 + 10
        } else {
            14 + 32 + rows as i32 * 34 + 10 + mixer_h
        }
    }

    /// What a side panel shows, with its box at (x, my) and `pw` wide. The bar's own sound, Wi-Fi and brightness popups
    /// draw the same body, so they look and work like the start menu's panels.
    #[allow(clippy::too_many_arguments)]
    fn draw_side_body(&self, c: &mut Canvas, p: &mut Popup, side: Side, x: i32, my: i32, pw: i32, hover: Option<PHit>, with_title: bool) {
        let accent = self.pal.primary;
        let muted = with_alpha(self.pal.on_surface, 0.6);
        let title = Self::side_title(side);
        let (ix, iw) = (x + 14, pw - 28);
        let mut cy = my + 14;
        let row = |c: &mut Canvas, hits: &mut Vec<(Rect, PHit)>, y: i32, active: bool, hot: bool, left: &str, right: &str, hit: PHit| {
            let r = (ix, y, ix + iw, y + 30);
            let fill = if active { accent } else if hot { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
            c.rounded_rect(r.0, r.1, iw, 30, 9.0, fill);
            let fg = if active { self.pal.on_primary } else { self.pal.on_surface };
            let rw = if right.is_empty() { 0 } else { self.text.width(right) + 10 };
            self.text.draw(c, ix + 10, y + 15, &self.text.fit(left, iw - 20 - rw), fg);
            if !right.is_empty() {
                self.text.draw(c, ix + iw - rw, y + 15, right, fg);
            }
            hits.push((r, hit));
        };
        if matches!(side, Side::Night | Side::Brightness) {
            self.draw_brightness_panel(c, p, x, my, pw, hover);
            return;
        }
        if side == Side::Wifi {
            self.draw_wifi_panel(c, p, x, my, pw, hover);
            return;
        }
        if matches!(side, Side::Sound | Side::Volume) {
            self.draw_sound_panel(c, p, x, my, pw, hover);
            return;
        }
        if side == Side::Mic {
            self.draw_mic_panel(c, p, x, my, pw, hover);
            return;
        }
        if with_title {
            self.text.draw(c, ix, cy + 8, title, accent);
            cy += 32;
        }
        match side {
            Side::Wifi => {
                if let Some(pr) = p.prompt.clone() {
                    self.draw_prompt(c, ix, iw, cy, &pr, hover, &mut p.hits);
                    return;
                }
                if p.nets.is_empty() {
                    let msg = p.nets_error.clone().unwrap_or_else(|| "No networks found".into());
                    self.text.draw(c, ix, cy + 14, &self.text.fit(&msg, iw), muted);
                }
                for (i, n) in p.nets.clone().iter().enumerate() {
                    let left = if n.secure { format!("\u{f023} {}", n.ssid) } else { n.ssid.clone() };
                    row(c, &mut p.hits, cy, n.active, hover == Some(PHit::Net(i)), &left, &format!("{}%", n.signal), PHit::Net(i));
                    cy += 34;
                }
            }
            Side::Bluetooth => {}
            Side::Sound => {
                if p.sinks.is_empty() {
                    self.text.draw(c, ix, cy + 14, "No output devices", muted);
                }
                for (i, s) in p.sinks.clone().iter().enumerate().take(6) {
                    row(c, &mut p.hits, cy, s.default, hover == Some(PHit::Sink(i)), &s.label, "", PHit::Sink(i));
                    cy += 34;
                }
            }
            Side::Mic => {
                // push to talk: the microphone stays muted unless the mic key is held
                let hot = hover == Some(PHit::Ptt);
                row(c, &mut p.hits, cy, p.ptt, hot, "Push to talk", if p.ptt { "on" } else { "off" }, PHit::Ptt);
                cy += 34;
                let key = p.ptt_key.clone();
                row(c, &mut p.hits, cy, false, hover == Some(PHit::PttLearn), "Set the key\u{2026}", &key, PHit::PttLearn);
                cy += 34;
                if p.sources.is_empty() {
                    self.text.draw(c, ix, cy + 14, "No microphones", muted);
                }
                for (i, s) in p.sources.clone().iter().enumerate().take(6) {
                    row(c, &mut p.hits, cy, s.default, hover == Some(PHit::Source(i)), &s.label, "", PHit::Source(i));
                    cy += 34;
                }
            }
            Side::Night | Side::Brightness => {}
            Side::Volume => {
                // mute first, then the output devices
                let is_muted = !p.quick.sound;
                row(c, &mut p.hits, cy, is_muted, hover == Some(PHit::Mute), "Mute", if is_muted { "on" } else { "" }, PHit::Mute);
                cy += 34;
                let boost = self.volume_max > 100;
                row(c, &mut p.hits, cy, boost, hover == Some(PHit::Boost), "Boost above 100%", if boost { "up to 150%" } else { "off" }, PHit::Boost);
                cy += 34;
                if p.sinks.is_empty() {
                    self.text.draw(c, ix, cy + 14, "No output devices", muted);
                }
                for (i, s) in p.sinks.clone().iter().enumerate().take(6) {
                    row(c, &mut p.hits, cy, s.default, hover == Some(PHit::Sink(i)), &s.label, "", PHit::Sink(i));
                    cy += 34;
                }
                // the mixer: each app that is playing sound, with its own slider
                cy += 4;
                self.text.draw(c, ix, cy + 12, "Apps", accent);
                cy += 30;
                if p.apps.is_empty() {
                    self.text.draw(c, ix, cy + 8, "No app is playing sound", muted);
                }
                for (i, a) in p.apps.clone().iter().enumerate().take(6) {
                    let label = self.text.fit(&a.label, iw - 56);
                    self.text.resized(13.0).draw(c, ix, cy + 8, &label, if a.muted { muted } else { self.pal.on_surface });
                    let v = format!("{}%", a.percent);
                    self.text.resized(13.0).draw(c, ix + iw - self.text.width(&v), cy + 8, &v, muted);
                    let ty = cy + 22;
                    c.rounded_rect(ix, ty, iw, 6, 3.0, with_alpha(accent, 0.25));
                    let filled = (iw as f32 * a.percent.min(100) as f32 / 100.0) as i32;
                    if filled > 0 {
                        c.rounded_rect(ix, ty, filled.max(6), 6, 3.0, accent);
                    }
                    c.circle(ix + filled, ty + 3, 6, accent);
                    p.hits.push(((ix - 8, cy + 16, ix + iw + 8, cy + 38), PHit::Slider(Slider::App(i))));
                    cy += 46;
                }
            }
        }
    }

    /// Title row of the sound and microphone panels: a glyph, the title and a thin line under it. Returns the height used.
    /// Height of the start menu for what there is to show.
    fn start_height(&self, _p: &Popup, sys: &Sys) -> i32 {
        14 + 52 + 24 + 4 + (4 * 58 + 3 * 8 + 10) + (56 + 14) + 40 + 40 + 8 + if sys.battery.is_some() { 58 + 10 } else { 0 } + 26 + 2 + (84 + 12) + 1 + 10 + 38 + 14
    }

    fn panel_head(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, glyph: &str, title: &str) -> i32 {
        let accent = self.pal.primary;
        self.text.resized(18.0).draw_centered(c, ix + 10, y + 14, glyph, accent);
        self.text.resized(17.0).draw(c, ix + 32, y + 14, title, self.pal.on_surface);
        c.fill_rect(ix, y + 32, iw, 1, with_alpha(self.pal.on_surface, 0.14));
        40
    }

    /// A small upper-case label such as OUTPUT. Returns the height used.
    fn section_label(&self, c: &mut Canvas, ix: i32, y: i32, text: &str) -> i32 {
        self.text.resized(11.0).draw(c, ix, y + 8, &text.to_uppercase(), with_alpha(self.pal.on_surface, 0.6));
        24
    }

    /// A title with a one-line description and an on/off switch on the right. Returns the height used.
    #[allow(clippy::too_many_arguments)]
    fn switch_row(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, title: &str, desc: &str, on: bool, hit: PHit, hover: Option<PHit>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        self.text.draw(c, ix, y + 11, title, self.pal.on_surface);
        let small = self.text.resized(11.5);
        let mut line = String::new();
        let mut ly = y + 31;
        let mut lines = 0;
        for word in desc.split_whitespace() {
            let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if small.width(&next) > iw - 64 && !line.is_empty() {
                small.draw(c, ix, ly, &line, with_alpha(self.pal.on_surface, 0.6));
                ly += 15;
                lines += 1;
                line = word.to_string();
                if lines == 2 {
                    break;
                }
            } else {
                line = next;
            }
        }
        if lines < 2 && !line.is_empty() {
            small.draw(c, ix, ly, &small.fit(&line, iw - 64), with_alpha(self.pal.on_surface, 0.6));
        }
        let (sw, sh) = (46, 26);
        let sx = ix + iw - sw;
        let sy = y + 6;
        let track = if on { accent } else { with_alpha(self.pal.on_surface, if hover == Some(hit) { 0.26 } else { 0.18 }) };
        c.rounded_rect(sx, sy, sw, sh, sh as f32 / 2.0, track);
        let kx = if on { sx + sw - 13 } else { sx + 13 };
        c.circle(kx, sy + sh / 2, 9, if on { self.pal.on_primary } else { self.pal.on_surface });
        hits.push(((ix, y, ix + iw, y + 44), hit));
        62
    }

    /// A slider with a glyph on the left and the percentage on the right. `max` is the value at the right end.
    #[allow(clippy::too_many_arguments)]
    fn icon_slider(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, glyph: &str, pct: u32, max: u32, id: Slider, icon_hit: Option<PHit>, hover: Option<PHit>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        let pct = pct.min(max);
        let lit = icon_hit.is_some() && hover == icon_hit;
        if lit {
            c.rounded_rect(ix - 4, y + 4, 28, 28, 14.0, with_alpha(accent, 0.2));
        }
        self.text.resized(17.0).draw_centered(c, ix + 10, y + 18, glyph, self.pal.on_surface);
        if let Some(h) = icon_hit {
            hits.push(((ix - 4, y + 4, ix + 24, y + 32), h));
        }
        let value = format!("{pct}%");
        let vw = self.text.width(&value);
        self.text.draw(c, ix + iw - vw, y + 18, &value, if pct > 100 { self.pal.error } else { self.pal.on_surface });
        let (tx, tw) = (ix + 34, iw - 34 - vw - 12);
        let ty = y + 14;
        c.rounded_rect(tx, ty, tw, 8, 4.0, with_alpha(accent, 0.25));
        let filled = (tw as f32 * pct as f32 / max as f32) as i32;
        if filled > 0 {
            c.rounded_rect(tx, ty, filled.max(8), 8, 4.0, if pct > 100 { self.pal.error } else { accent });
        }
        if max > 100 {
            let mx = tx + (tw as f32 * 100.0 / max as f32) as i32;
            c.rounded_rect(mx - 1, ty - 4, 2, 16, 1.0, with_alpha(self.pal.on_surface, 0.6));
        }
        c.circle(tx + filled, ty + 4, 8, accent);
        hits.push(((tx - 8, y + 4, tx + tw + 8, y + 32), PHit::Slider(id)));
        40
    }

    /// A device row: highlighted with a tick when it is the one in use.
    fn device_row(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, label: &str, active: bool, hit: PHit, hover: Option<PHit>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        let hot = hover == Some(hit);
        let fill = if active { with_alpha(accent, 0.2) } else if hot { with_alpha(self.pal.on_surface, 0.12) } else { with_alpha(self.pal.on_surface, 0.06) };
        c.rounded_rect(ix, y, iw, 34, 10.0, fill);
        if active {
            c.rounded_outline(ix, y, iw, 34, 10.0, 1.0, with_alpha(accent, 0.5));
        }
        self.text.draw(c, ix + 12, y + 17, &self.text.fit(label, iw - 56), self.pal.on_surface);
        if active {
            self.text.resized(15.0).draw_centered(c, ix + iw - 20, y + 17, "\u{f012c}", self.pal.on_surface_variant);
        }
        hits.push(((ix, y, ix + iw, y + 34), hit));
        38
    }

    /// The Wi-Fi panel: the radio switch and a refresh button, then the networks with their signal; a secured one you are not
    /// saved on asks for its password first.
    fn draw_wifi_panel(&self, c: &mut Canvas, p: &mut Popup, x: i32, my: i32, pw: i32, hover: Option<PHit>) {
        let accent = self.pal.primary;
        let muted_c = with_alpha(self.pal.on_surface, 0.6);
        let (ix, iw) = (x + 16, pw - 32);
        let mut cy = my + 14;
        let on = p.quick.wifi;
        self.text.resized(18.0).draw_centered(c, ix + 10, cy + 14, if on { "\u{f05a9}" } else { "\u{f05aa}" }, accent);
        self.text.resized(17.0).draw(c, ix + 32, cy + 14, "Wi-Fi", self.pal.on_surface);
        // the switch on the far right, the refresh button beside it
        let (sw, sh) = (46, 26);
        let sx = ix + iw - sw;
        let track = if on { accent } else { with_alpha(self.pal.on_surface, if hover == Some(PHit::Tile(crate::quick::Tile::Wifi)) { 0.26 } else { 0.18 }) };
        c.rounded_rect(sx, cy + 1, sw, sh, sh as f32 / 2.0, track);
        c.circle(if on { sx + sw - 13 } else { sx + 13 }, cy + 1 + sh / 2, 9, if on { self.pal.on_primary } else { self.pal.on_surface });
        p.hits.push(((sx, cy, sx + sw, cy + 28), PHit::Tile(crate::quick::Tile::Wifi)));
        let rr = (sx - 40, cy, sx - 8, cy + 28);
        if hover == Some(PHit::WifiRefresh) {
            c.rounded_rect(rr.0, rr.1, 32, 28, 9.0, with_alpha(self.pal.on_surface, 0.12));
        }
        self.text.resized(17.0).draw_centered(c, rr.0 + 16, cy + 14, "\u{f0450}", accent);
        p.hits.push((rr, PHit::WifiRefresh));
        c.fill_rect(ix, cy + 32, iw, 1, with_alpha(self.pal.on_surface, 0.14));
        cy += 40;
        if let Some(pr) = p.prompt.clone() {
            self.draw_prompt(c, ix, iw, cy, &pr, hover, &mut p.hits);
            return;
        }
        cy += self.section_label(c, ix, cy, "Networks");
        if p.nets.is_empty() {
            let msg = p.nets_error.clone().unwrap_or_else(|| if on { "No networks found".into() } else { "Wi-Fi is off".into() });
            self.text.draw(c, ix, cy + 14, &self.text.fit(&msg, iw), muted_c);
        }
        for (i, n) in p.nets.clone().iter().enumerate() {
            let hot = hover == Some(PHit::Net(i));
            let fill = if n.active { with_alpha(accent, 0.2) } else if hot { with_alpha(self.pal.on_surface, 0.12) } else { with_alpha(self.pal.on_surface, 0.0) };
            c.rounded_rect(ix, cy, iw, 36, 10.0, fill);
            if n.active {
                c.rounded_outline(ix, cy, iw, 36, 10.0, 1.0, with_alpha(accent, 0.5));
            }
            let bars = match n.signal {
                0..=25 => "\u{f091f}",
                26..=50 => "\u{f0922}",
                51..=75 => "\u{f0925}",
                _ => "\u{f0928}",
            };
            self.text.resized(18.0).draw_centered(c, ix + 20, cy + 18, bars, if n.active { accent } else { self.pal.on_surface });
            self.text.draw(c, ix + 40, cy + 18, &self.text.fit(&n.ssid, iw - 40 - 36), self.pal.on_surface);
            if n.active {
                self.text.resized(15.0).draw_centered(c, ix + iw - 18, cy + 18, "\u{f012c}", self.pal.on_surface_variant);
            } else if n.secure {
                self.text.resized(14.0).draw_centered(c, ix + iw - 18, cy + 18, "\u{f033e}", muted_c);
            }
            p.hits.push(((ix, cy, ix + iw, cy + 36), PHit::Net(i)));
            cy += 40;
        }
    }

    /// The brightness panel: screen brightness, then the night light switch and its strength.
    fn draw_brightness_panel(&self, c: &mut Canvas, p: &mut Popup, x: i32, my: i32, pw: i32, hover: Option<PHit>) {
        let (ix, iw) = (x + 16, pw - 32);
        let mut cy = my + 14;
        cy += self.panel_head(c, ix, iw, cy, "\u{f00df}", "Brightness");
        match p.bright {
            Some(b) => cy += self.icon_slider(c, ix, iw, cy, "\u{f00df}", b, 100, Slider::Brightness, None, hover, &mut p.hits),
            None => {
                self.text.draw(c, ix, cy + 18, &self.text.fit("No adjustable screen backlight", iw), with_alpha(self.pal.on_surface, 0.6));
                cy += 40;
            }
        }
        cy += 4;
        cy += self.switch_row(c, ix, iw, cy, "Night light", "Warms the screen colours in the evening.", p.quick.night, PHit::Tile(crate::quick::Tile::NightLight), hover, &mut p.hits);
        cy += 2;
        self.icon_slider(c, ix, iw, cy, "\u{f0594}", p.night_strength, 100, Slider::Night, None, hover, &mut p.hits);
    }

    /// The sound panel: volume slider, boost, output devices and the per-app mixer.
    fn draw_sound_panel(&self, c: &mut Canvas, p: &mut Popup, x: i32, my: i32, pw: i32, hover: Option<PHit>) {
        let muted_c = with_alpha(self.pal.on_surface, 0.6);
        let (ix, iw) = (x + 16, pw - 32);
        let mut cy = my + 14;
        cy += self.panel_head(c, ix, iw, cy, "\u{f057e}", "Sound");
        let (pct, is_muted) = p.vol;
        let boost = self.volume_max > 100;
        let max = if boost { 150 } else { 100 }.max(pct.min(150));
        let glyph = if is_muted { "\u{f0581}" } else { "\u{f057e}" };
        cy += self.icon_slider(c, ix, iw, cy, glyph, pct, max, Slider::Volume, Some(PHit::Mute), hover, &mut p.hits);
        cy += 4;
        cy += self.switch_row(c, ix, iw, cy, "Boost volume", "Allow up to 150 %. Above 100 % can distort.", boost, PHit::Boost, hover, &mut p.hits);
        cy += 2;
        cy += self.section_label(c, ix, cy, "Output");
        if p.sinks.is_empty() {
            self.text.draw(c, ix, cy + 14, "No output devices", muted_c);
            cy += 30;
        }
        for (i, s) in p.sinks.clone().iter().enumerate().take(6) {
            cy += self.device_row(c, ix, iw, cy, &s.label, s.default, PHit::Sink(i), hover, &mut p.hits);
        }
        cy += 6;
        cy += self.section_label(c, ix, cy, "Apps");
        if p.apps.is_empty() {
            self.text.draw(c, ix, cy + 8, "No app is playing sound right now", muted_c);
        }
        let accent = self.pal.primary;
        for (i, a) in p.apps.clone().iter().enumerate().take(6) {
            let label = self.text.fit(&a.label, iw - 56);
            self.text.resized(13.0).draw(c, ix, cy + 8, &label, if a.muted { muted_c } else { self.pal.on_surface });
            let v = format!("{}%", a.percent);
            self.text.resized(13.0).draw(c, ix + iw - self.text.width(&v), cy + 8, &v, muted_c);
            let ty = cy + 22;
            c.rounded_rect(ix, ty, iw, 6, 3.0, with_alpha(accent, 0.25));
            let filled = (iw as f32 * a.percent.min(100) as f32 / 100.0) as i32;
            if filled > 0 {
                c.rounded_rect(ix, ty, filled.max(6), 6, 3.0, accent);
            }
            c.circle(ix + filled, ty + 3, 6, accent);
            p.hits.push(((ix - 8, cy + 16, ix + iw + 8, cy + 38), PHit::Slider(Slider::App(i))));
            cy += 46;
        }
    }

    /// The microphone panel: level, push to talk and the input devices.
    fn draw_mic_panel(&self, c: &mut Canvas, p: &mut Popup, x: i32, my: i32, pw: i32, hover: Option<PHit>) {
        let muted_c = with_alpha(self.pal.on_surface, 0.6);
        let (ix, iw) = (x + 16, pw - 32);
        let mut cy = my + 14;
        let live = p.quick.mic;
        cy += self.panel_head(c, ix, iw, cy, if live { "\u{f036c}" } else { "\u{f036d}" }, "Microphone");
        let glyph = if live { "\u{f036c}" } else { "\u{f036d}" };
        let shown = if live { p.mic_percent } else { 0 };
        cy += self.icon_slider(c, ix, iw, cy, glyph, shown, 100, Slider::Mic, Some(PHit::Tile(crate::quick::Tile::Mic)), hover, &mut p.hits);
        cy += 4;
        let key = if p.ptt_key.is_empty() { "the mic key".to_string() } else { format!("the mic key ({})", p.ptt_key) };
        cy += self.switch_row(c, ix, iw, cy, "Push to talk", &format!("Mic stays muted; hold {key} to talk."), p.ptt, PHit::Ptt, hover, &mut p.hits);
        cy += 2;
        let learn = (ix, cy, ix + iw, cy + 30);
        let hot = hover == Some(PHit::PttLearn);
        c.rounded_rect(ix, cy, iw, 30, 9.0, with_alpha(self.pal.on_surface, if hot { 0.16 } else { 0.08 }));
        self.text.draw(c, ix + 10, cy + 15, "Set the key\u{2026}", self.pal.on_surface);
        if !p.ptt_key.is_empty() {
            self.text.draw(c, ix + iw - 10 - self.text.width(&p.ptt_key), cy + 15, &p.ptt_key, muted_c);
        }
        p.hits.push((learn, PHit::PttLearn));
        cy += 38;
        cy += self.section_label(c, ix, cy, "Input");
        if p.sources.is_empty() {
            self.text.draw(c, ix, cy + 14, "No microphones", muted_c);
        }
        for (i, s) in p.sources.clone().iter().enumerate().take(6) {
            cy += self.device_row(c, ix, iw, cy, &s.label, s.default, PHit::Source(i), hover, &mut p.hits);
        }
    }

    /// The password prompt for `pr` at (ix, y), `iw` wide. Returns the height used.
    fn draw_prompt(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, pr: &Prompt, hover: Option<PHit>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        self.text.draw(c, ix, y + 10, &self.text.fit(&format!("Password for {}", pr.ssid), iw), accent);
        let field = (ix, y + 24, ix + iw, y + 24 + 34);
        c.rounded_rect(field.0, field.1, iw, 34, 9.0, with_alpha(self.pal.on_surface, 0.10));
        c.rounded_outline(field.0, field.1, iw, 34, 9.0, 1.5, accent);
        let dots: String = "\u{2022}".repeat(pr.text.chars().count().min(40));
        self.text.draw(c, ix + 12, field.1 + 17, &format!("{dots}\u{258f}"), self.pal.on_surface);
        let msg = if pr.busy { Some(("Connecting\u{2026}".to_string(), with_alpha(self.pal.on_surface, 0.6))) } else { pr.error.clone().map(|e| (e, self.pal.error)) };
        if let Some((m, col)) = msg {
            self.text.draw(c, ix, field.3 + 13, &self.text.fit(&m, iw), col);
        }
        let by = y + 24 + 34 + 26;
        let half = (iw - 8) / 2;
        for (i, (label, hit)) in [("Connect", PHit::WifiConnect), ("Cancel", PHit::WifiCancel)].into_iter().enumerate() {
            let bx = ix + i as i32 * (half + 8);
            let r = (bx, by, bx + half, by + 32);
            let primary = i == 0;
            let fill = if primary { accent } else if hover == Some(hit) { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
            c.rounded_rect(r.0, r.1, half, 32, 9.0, fill);
            self.text.draw_centered(c, bx + half / 2, by + 16, label, if primary { self.pal.on_primary } else { self.pal.on_surface });
            hits.push((r, hit));
        }
        PROMPT_H
    }

    /// Paint the box of a popup like the notification cards: the bar's corner radius, border and see-through level (never
    /// clearer than 40 % opaque), with the fill's alpha applied once.
    fn panel(&self, c: &mut Canvas, x: i32, y: i32, w: i32, h: i32) {
        let a = self.bg_alpha.max((40u32 * 255 / 100) as u8);
        let mut border = self.style.border.unwrap_or(self.pal.primary);
        if self.style.border_blend {
            border[3] = (border[3] as u32 * a as u32 / 255) as u8;
        }
        let fill = vesper_config::theme::alpha(self.pal.surface, a);
        let r = self.style.radius.clamp(8, 28);
        shape::paint_exact(c, &Shape::Round { x, y, w, h, r }, fill, self.style.border_width.max(1), border);
    }

    /// A labelled slider row at (ix, y): the label on the left, the percentage on the right, the track below.
    /// Records the grab area and returns the height it used.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn slider_row(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, label: &str, pct: u32, dim: bool, id: Slider, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        self.slider_row_with(c, ix, iw, y, label, pct, dim, id, None, hits)
    }

    /// A slider row with an optional small arrow beside the percentage that opens a side panel: `(panel, highlighted)`.
    #[allow(clippy::too_many_arguments)]
    fn slider_row_with(&self, c: &mut Canvas, ix: i32, iw: i32, y: i32, label: &str, pct: u32, dim: bool, id: Slider, chev: Option<(Side, bool)>, hits: &mut Vec<(Rect, PHit)>) -> i32 {
        let accent = self.pal.primary;
        // the volume slider goes to 150% with boost on (or when the volume already is over 100%)
        let max = if id == Slider::Volume { self.volume_max.max(100).max(pct.min(150)) } else { 100 };
        let pct = pct.min(max);
        self.text.draw(c, ix, y + 12, label, accent);
        let value = format!("{pct}%");
        let value_colour = if dim { with_alpha(self.pal.on_surface, 0.6) } else if pct > 100 { self.pal.error } else { self.pal.on_surface };
        self.text.draw(c, ix + iw - self.text.width(&value), y + 12, &value, value_colour);
        if let Some((side, lit)) = chev {
            let cx = ix + iw - self.text.width(&value) - 22;
            if lit {
                c.rounded_rect(cx - 11, y + 1, 22, 22, 11.0, with_alpha(accent, 0.25));
            }
            self.text.resized(13.0).draw_centered(c, cx, y + 12, "\u{f0142}", accent);
            hits.push(((cx - 13, y - 2, cx + 13, y + 26), PHit::Open(side)));
        }
        let ty = y + 30 + 12;
        c.rounded_rect(ix, ty, iw, 8, 4.0, with_alpha(accent, 0.25));
        let filled = (iw as f32 * pct as f32 / max as f32) as i32;
        if filled > 0 {
            c.rounded_rect(ix, ty, filled.max(8), 8, 4.0, if dim { with_alpha(accent, 0.45) } else if pct > 100 { self.pal.error } else { accent });
        }
        if max > 100 {
            // a tick where 100% is
            let tx = ix + (iw as f32 * 100.0 / max as f32) as i32;
            c.rounded_rect(tx - 1, ty - 4, 2, 16, 1.0, with_alpha(self.pal.on_surface, 0.6));
        }
        c.circle(ix + filled, ty + 4, 8, accent);
        hits.push(((ix - 8, y + 30, ix + iw + 8, y + 62), PHit::Slider(id)));
        66
    }

    /// The on-screen popup for a volume or brightness change: an icon, a bar and the percentage, in the bar's look.
    pub fn draw_osd(&mut self, canvas: &mut [u8], w: u32, h: u32, kind: &str, pct: u32, muted: bool) {
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let accent = self.pal.primary;
        self.panel(&mut c, 0, 0, w as i32, h as i32);
        let max = if kind == "volume" { self.volume_max.max(100).max(pct.min(150)) } else { 100 };
        let pct = pct.min(max);
        let glyph = if kind == "volume" {
            if muted || pct == 0 { "\u{f0581}" } else { "\u{f057e}" }
        } else {
            "\u{f00e0}"
        };
        let col = if muted { with_alpha(accent, 0.45) } else if pct > 100 { self.pal.error } else { accent };
        self.text.resized(22.0).draw_centered(&mut c, 34, h as i32 / 2, glyph, col);
        let value = if muted { "muted".to_string() } else { format!("{pct}%") };
        let vw = self.text.width("150%").max(self.text.width(&value));
        self.text.draw(&mut c, w as i32 - 20 - vw + (vw - self.text.width(&value)), h as i32 / 2, &value, if muted { with_alpha(self.pal.on_surface, 0.6) } else { self.pal.on_surface });
        let (tx, tw) = (62, w as i32 - 62 - 28 - vw);
        let ty = h as i32 / 2 - 4;
        c.rounded_rect(tx, ty, tw, 8, 4.0, with_alpha(accent, 0.25));
        let filled = (tw as f32 * pct as f32 / max as f32) as i32;
        if filled > 0 {
            c.rounded_rect(tx, ty, filled.max(8), 8, 4.0, col);
        }
        if max > 100 {
            let mark = tx + (tw as f32 * 100.0 / max as f32) as i32;
            c.rounded_rect(mark - 1, ty - 4, 2, 16, 1.0, with_alpha(self.pal.on_surface, 0.6));
        }
    }

    /// Draw the popup into the overlay canvas (`w` x `h` pixels) and record its clickable parts.
    pub fn draw_popup(&mut self, canvas: &mut [u8], w: u32, h: u32, p: &mut Popup, sys: &Sys, hover: Option<PHit>) {
        p.bright = sys.brightness;
        p.vol = sys.volume.map_or((0, false), |v| (v.percent, v.muted));
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let accent = self.pal.primary;
        let (bw, bh) = match p.kind {
            Kind::Brightness => (340, self.side_height(p, Side::Brightness)),
            Kind::Start => (400, self.start_height(p, sys)),
            Kind::Calendar => {
                let first = month_start(p.month_offset);
                let lead = first.weekday().num_days_from_monday() as i32;
                let days = (month_start(p.month_offset + 1) - first).num_days() as i32;
                let rows = (lead + days + 6) / 7;
                (288, 14 + 38 + 26 + rows * 34 + 10)
            }
            Kind::Volume => (340, self.side_height(p, Side::Volume)),
            Kind::Battery => (320, 14 + 32 + 46 + 26 * p.battery_rows.len() as i32 + if p.profiles.is_empty() { 0 } else { 8 + 30 + 38 } + 10),
            Kind::Network => (340, self.side_height(p, Side::Wifi)),
            Kind::Drives => (320, 14 + 32 + 38 * sys.drives.len().max(1) as i32 + 10),
            Kind::TrayMenu => (260, 14 + Self::menu_height(p) + 10),
        };
        let x = (p.anchor - bw / 2).clamp(8, (w as i32 - bw - 8).max(8));
        let y = 6;
        p.rect = (x, y, bw, bh);
        p.hits.clear();
        self.panel(&mut c, x, y, bw, bh);
        let (ix, iw) = (x + 14, bw - 28);
        let mut cy = y + 14;
        let muted = with_alpha(self.pal.on_surface, 0.6);
        let button = |c: &mut Canvas, r: Rect, on: bool, hot: bool| {
            let col = if on { accent } else if hot { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
            c.rounded_rect(r.0, r.1, r.2 - r.0, r.3 - r.1, 9.0, col);
        };

        match p.kind {
            Kind::Start => {
                let dim = with_alpha(self.pal.on_surface, 0.6);
                let line = with_alpha(self.pal.on_surface, 0.12);
                // ---- who: an avatar with the first letter, the user and the machine, and the tools on the right
                let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
                let host = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_default();
                let initial = user.chars().next().map_or("?".to_string(), |ch| ch.to_uppercase().to_string());
                c.circle(ix + 22, cy + 22, 22, with_alpha(accent, 0.18));
                c.circle(ix + 22, cy + 22, 21, self.pal.surface_container_high);
                c.rounded_outline(ix, cy, 44, 44, 22.0, 1.5, accent);
                self.text.resized(18.0).draw_centered(&mut c, ix + 22, cy + 22, &initial, accent);
                self.text.resized(16.0).draw(&mut c, ix + 56, cy + 14, &user, self.pal.on_surface);
                self.text.resized(12.0).draw(&mut c, ix + 56, cy + 33, &host, dim);
                let tools = [(StartAct::Settings, "\u{f0493}"), (StartAct::Apps, "\u{f0570}"), (StartAct::Clipboard, "\u{f0ea}"), (StartAct::Wallpapers, "\u{f02e9}")];
                let mut bx = ix + iw;
                for (act, glyph) in tools {
                    bx -= 36;
                    let r = (bx, cy + 4, bx + 36, cy + 40);
                    if hover == Some(PHit::Start(act)) {
                        c.rounded_rect(r.0, r.1, 36, 36, 10.0, with_alpha(self.pal.on_surface, 0.12));
                    }
                    self.text.resized(17.0).draw_centered(&mut c, bx + 18, cy + 22, glyph, self.pal.on_surface);
                    p.hits.push((r, PHit::Start(act)));
                    bx -= 2;
                }
                cy += 52;
                cy += self.section_label(&mut c, ix, cy, "Quick settings") + 4;
                // ---- two columns of switches: icon, name, state; a small arrow on those with a panel
                let (gap, th) = (8, 58);
                let tw = (iw - gap) / 2;
                let vol_text = match sys.volume {
                    Some(v) if !v.muted => format!("{}%", v.percent),
                    _ => "Muted".to_string(),
                };
                for (i, (tile, _short, on_glyph, off_glyph)) in crate::quick::TILES.iter().enumerate() {
                    let (col, row) = (i as i32 % 2, i as i32 / 2);
                    let (tx, ty) = (ix + col * (tw + gap), cy + row * (th + gap));
                    let on = p.quick.get(*tile);
                    let hot = hover == Some(PHit::Tile(*tile));
                    let (title, state): (&str, String) = match tile {
                        crate::quick::Tile::Wifi => ("Wi-Fi", if on { "On" } else { "Off" }.into()),
                        crate::quick::Tile::Bluetooth => ("Bluetooth", if on { "On" } else { "Off" }.into()),
                        crate::quick::Tile::Airplane => ("Airplane mode", if on { "On" } else { "Off" }.into()),
                        crate::quick::Tile::NightLight => ("Night light", if on { "On" } else { "Off" }.into()),
                        crate::quick::Tile::Sound => ("Sound", vol_text.clone()),
                        crate::quick::Tile::Mic => ("Microphone", if on { "On" } else { "Muted" }.into()),
                        crate::quick::Tile::Dnd => ("Do not disturb", if on { "On" } else { "Off" }.into()),
                        crate::quick::Tile::KeepAwake => ("Keep awake", if on { "On" } else { "Off" }.into()),
                    };
                    let fill = if on { with_alpha(accent, if hot { 0.28 } else { 0.18 }) } else { with_alpha(self.pal.on_surface, if hot { 0.12 } else { 0.06 }) };
                    c.rounded_rect(tx, ty, tw, th, 12.0, fill);
                    c.rounded_outline(tx, ty, tw, th, 12.0, 1.0, if on { accent } else { line });
                    self.text.resized(22.0).draw_centered(&mut c, tx + 28, ty + th / 2, if on { on_glyph } else { off_glyph }, if on { accent } else { self.pal.on_surface });
                    self.text.resized(14.5).draw(&mut c, tx + 54, ty + 22, &self.text.resized(14.5).fit(title, tw - 54 - if side_for(*tile).is_some_and(|s| s != Side::Night) { 30 } else { 12 }), self.pal.on_surface);
                    self.text.resized(12.0).draw(&mut c, tx + 54, ty + 41, &state, dim);
                    if let Some(side) = side_for(*tile).filter(|s| *s != Side::Night) {
                        let cr = (tx + tw - 34, ty, tx + tw, ty + th);
                        if p.side == Some(side) || hover == Some(PHit::Open(side)) {
                            c.rounded_rect(cr.0 + 6, cr.1 + th / 2 - 12, 24, 24, 12.0, with_alpha(self.pal.on_surface, 0.14));
                        }
                        self.text.resized(15.0).draw_centered(&mut c, cr.0 + 18, ty + th / 2, "\u{f0142}", self.pal.on_surface);
                        p.hits.push((cr, PHit::Open(side)));
                    }
                    p.hits.push(((tx, ty, tx + tw, ty + th), PHit::Tile(*tile)));
                }
                cy += 4 * th + 3 * gap + 10;
                // ---- three tools
                let bw3 = (iw - 16) / 3;
                for (i, (act, label, glyph)) in [(StartAct::Screenshot, "Screenshot", "\u{f0100}"), (StartAct::ReadQr, "Scan QR", "\u{f0432}"), (StartAct::CopyText, "Scan text", "\u{f113d}")].into_iter().enumerate() {
                    let bx = ix + i as i32 * (bw3 + 8);
                    let hot = hover == Some(PHit::Start(act));
                    c.rounded_rect(bx, cy, bw3, 56, 12.0, with_alpha(self.pal.on_surface, if hot { 0.12 } else { 0.06 }));
                    c.rounded_outline(bx, cy, bw3, 56, 12.0, 1.0, line);
                    self.text.resized(19.0).draw_centered(&mut c, bx + bw3 / 2, cy + 18, glyph, self.pal.on_surface);
                    self.text.resized(12.5).draw_centered(&mut c, bx + bw3 / 2, cy + 40, label, self.pal.on_surface);
                    p.hits.push(((bx, cy, bx + bw3, cy + 56), PHit::Start(act)));
                }
                cy += 56 + 14;
                // ---- brightness and volume
                let lit_b = p.side == Some(Side::Brightness);
                cy += self.icon_slider(&mut c, ix, iw, cy, "\u{f00df}", sys.brightness.unwrap_or(0), 100, Slider::Brightness, Some(PHit::Open(Side::Brightness)), if lit_b { Some(PHit::Open(Side::Brightness)) } else { hover }, &mut p.hits);
                let (vp, vm) = sys.volume.map_or((0, false), |v| (v.percent, v.muted));
                let vmax = if self.volume_max > 100 { 150 } else { 100 }.max(vp.min(150));
                cy += self.icon_slider(&mut c, ix, iw, cy, if vm { "\u{f0581}" } else { "\u{f057e}" }, vp, vmax, Slider::Volume, Some(PHit::Mute), hover, &mut p.hits);
                cy += 8;
                // ---- the battery
                if let Some(b) = sys.battery {
                    c.rounded_rect(ix, cy, iw, 58, 12.0, with_alpha(self.pal.on_surface, 0.06));
                    c.rounded_outline(ix, cy, iw, 58, 12.0, 1.0, line);
                    self.text.resized(22.0).draw_centered(&mut c, ix + 26, cy + 29, "\u{f0079}", self.pal.on_surface);
                    self.text.resized(15.0).draw(&mut c, ix + 46, cy + 20, &format!("{}%", b.percent), self.pal.on_surface);
                    let find = |k: &str| p.battery_rows.iter().find(|(l, _)| l == k).map(|(_, v)| v.clone());
                    let status = find("Status").unwrap_or_default().to_lowercase();
                    let sub = match find("Time left").or_else(|| find("Until full")) {
                        Some(t) => format!("{status} \u{b7} {t}"),
                        None => status,
                    };
                    self.text.resized(11.5).draw(&mut c, ix + 46, cy + 40, &self.text.resized(11.0).fit(&sub, iw - 46 - 140), dim);
                    let n = p.profiles.len() as i32;
                    if n > 0 {
                        let pw3 = 36;
                        let mut px = ix + iw - 10 - n * pw3 - (n - 1) * 6;
                        for (i, name) in p.profiles.iter().enumerate() {
                            let on = *name == p.profile;
                            let r = (px, cy + 14, px + pw3, cy + 44);
                            let hot = hover == Some(PHit::Profile(i));
                            c.rounded_rect(r.0, r.1, pw3, 30, 9.0, if on { with_alpha(accent, 0.25) } else { with_alpha(self.pal.on_surface, if hot { 0.14 } else { 0.06 }) });
                            c.rounded_outline(r.0, r.1, pw3, 30, 9.0, 1.0, if on { accent } else { line });
                            // icons: a leaf (saves the most battery), a balance, a bolt (full speed)
                            let glyph = match name.as_str() {
                                "power-saver" => "\u{f06c}",
                                "performance" => "\u{f0e7}",
                                _ => "\u{f24e}",
                            };
                            self.text.resized(15.0).draw_ink_centered(&mut c, px + pw3 / 2, cy + 29, glyph, if on { accent } else { self.pal.on_surface });
                            p.hits.push((r, PHit::Profile(i)));
                            px += pw3 + 6;
                        }
                    }
                    cy += 58 + 10;
                }
                // ---- performance: two cards with a graph
                cy += self.section_label(&mut c, ix, cy, "Performance") + 2;
                let cw = (iw - 10) / 2;
                let h = &sys.hist;
                let gib = |kib: u64| kib as f64 / 1024.0 / 1024.0;
                let cards: [(&str, String, u32, &Vec<u32>); 2] = [
                    ("CPU", "load".to_string(), sys.stats.map_or(0, |s| s.cpu), &h.cpu),
                    ("RAM", format!("{:.1} / {:.1} GiB", gib(h.mem_used_kib), gib(h.mem_total_kib)), sys.stats.map_or(0, |s| s.mem), &h.mem),
                ];
                for (i, (name, sub, pct, hist)) in cards.iter().enumerate() {
                    let cx0 = ix + i as i32 * (cw + 10);
                    c.rounded_rect(cx0, cy, cw, 84, 12.0, with_alpha(self.pal.on_surface, 0.06));
                    c.rounded_outline(cx0, cy, cw, 84, 12.0, 1.0, line);
                    self.text.resized(12.0).draw(&mut c, cx0 + 12, cy + 16, name, accent);
                    self.text.resized(11.0).draw(&mut c, cx0 + 12, cy + 33, &self.text.resized(11.0).fit(sub, cw - 24 - 60), dim);
                    let big = format!("{pct}%");
                    let big_font = self.text.resized(22.0);
                    big_font.draw(&mut c, cx0 + cw - 12 - big_font.width(&big), cy + 24, &big, accent);
                    // the graph: the last samples as a filled area along the bottom of the card
                    let (gx, gy, gw, gh) = (cx0 + 8, cy + 44, cw - 16, 32);
                    let n = (gw / 3).max(2) as usize;
                    let tail: Vec<u32> = hist.iter().rev().take(n).rev().copied().collect();
                    for (k, v) in tail.iter().enumerate() {
                        let x = gx + gw - (tail.len() - k) as i32 * 3;
                        let bh = ((gh as f32) * (*v).min(100) as f32 / 100.0).round() as i32;
                        if bh > 0 {
                            c.fill_rect(x, gy + gh - bh, 3, bh, with_alpha(accent, 0.28));
                            c.fill_rect(x, gy + gh - bh, 3, 2, accent);
                        }
                    }
                }
                cy += 84 + 12;
                // ---- the power button
                c.fill_rect(ix, cy, iw, 1, line);
                cy += 10;
                let pr = (ix + iw - 40, cy, ix + iw, cy + 38);
                let hot = hover == Some(PHit::Start(StartAct::PowerMenu));
                c.rounded_rect(pr.0, pr.1, 40, 38, 11.0, with_alpha(self.pal.on_surface, if hot { 0.14 } else { 0.06 }));
                c.rounded_outline(pr.0, pr.1, 40, 38, 11.0, 1.0, line);
                self.text.resized(18.0).draw_centered(&mut c, pr.0 + 20, cy + 19, "\u{f0425}", self.pal.on_surface);
                p.hits.push((pr, PHit::Start(StartAct::PowerMenu)));
            }
            Kind::Calendar => {
                let first = month_start(p.month_offset);
                let title = first.format("%B %Y").to_string();
                self.text.draw(&mut c, ix, cy + 14, &title, accent);
                for (dir, label, bx) in [(-1, "\u{f053}", ix + iw - 64), (1, "\u{f054}", ix + iw - 28)] {
                    let r = (bx, cy, bx + 28, cy + 28);
                    button(&mut c, r, false, hover == Some(PHit::Month(dir)));
                    self.text.draw_centered(&mut c, bx + 14, cy + 14, label, accent);
                    p.hits.push((r, PHit::Month(dir)));
                }
                cy += 38;
                let cw = iw / 7;
                for (i, d) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
                    self.text.draw_centered(&mut c, ix + i as i32 * cw + cw / 2, cy + 10, d, muted);
                }
                cy += 26;
                let lead = first.weekday().num_days_from_monday() as i32;
                let days = (month_start(p.month_offset + 1) - first).num_days() as i32;
                let today = Local::now().date_naive();
                for day in 1..=days {
                    let idx = lead + day - 1;
                    let (col, row) = (idx % 7, idx / 7);
                    let (cx, ry) = (ix + col * cw + cw / 2, cy + row * 34);
                    let is_today = first.with_day(day as u32) == Some(today);
                    if is_today {
                        c.rounded_rect(cx - 15, ry, 30, 30, 15.0, accent);
                    }
                    let tc: Rgba = if is_today { self.pal.on_primary } else { self.pal.on_surface };
                    self.text.draw_centered(&mut c, cx, ry + 15, &day.to_string(), tc);
                }
            }
            Kind::Brightness => self.draw_side_body(&mut c, p, Side::Brightness, x, y, bw, hover, true),
            Kind::Volume => self.draw_side_body(&mut c, p, Side::Volume, x, y, bw, hover, true),
            Kind::Battery => {
                self.text.draw(&mut c, ix, cy + 8, "Battery", accent);
                cy += 32;
                // the charge: the percentage over a bar
                let percent = sys.battery.map(|b| b.percent.min(100));
                match percent {
                    Some(pc) => {
                        self.text.draw_centered(&mut c, ix + iw / 2, cy + 8, &format!("{pc}%"), accent);
                        c.rounded_rect(ix, cy + 22, iw, 10, 5.0, with_alpha(self.pal.on_surface, 0.12));
                        let fw = (iw * pc as i32 / 100).max(10);
                        c.rounded_rect(ix, cy + 22, fw, 10, 5.0, accent);
                    }
                    None => self.text.draw(&mut c, ix, cy + 14, "No battery", muted),
                }
                cy += 46;
                // label on the left, value in a second column
                let col = ix + iw * 42 / 100;
                for (label, value) in &p.battery_rows {
                    self.text.draw(&mut c, ix, cy + 10, label, self.pal.on_surface);
                    self.text.draw(&mut c, col, cy + 10, &self.text.fit(value, ix + iw - col), self.pal.on_surface);
                    cy += 26;
                }
                if !p.profiles.is_empty() {
                    cy += 8;
                    self.text.draw(&mut c, ix, cy + 8, "Power mode", muted);
                    cy += 30;

                    let n = p.profiles.len() as i32;
                    let bw2 = (iw - (n - 1) * 6) / n;
                    for (i, name) in p.profiles.iter().enumerate() {
                        let bx = ix + i as i32 * (bw2 + 6);
                        let r = (bx, cy, bx + bw2, cy + 32);
                        let on = *name == p.profile;
                        button(&mut c, r, on, hover == Some(PHit::Profile(i)));
                        let label = match name.as_str() {
                            "power-saver" => "Saver",
                            "balanced" => "Balanced",
                            "performance" => "Boost",
                            o => o,
                        };
                        self.text.draw_centered(&mut c, bx + bw2 / 2, cy + 16, label, if on { self.pal.on_primary } else { self.pal.on_surface });
                        p.hits.push((r, PHit::Profile(i)));
                    }
                }
            }
            Kind::Network => self.draw_side_body(&mut c, p, Side::Wifi, x, y, bw, hover, true),
            Kind::Drives => {
                self.text.draw(&mut c, ix, cy + 8, "Drives", accent);
                cy += 32;
                if sys.drives.is_empty() {
                    self.text.draw(&mut c, ix, cy + 14, "No removable drives", muted);
                }
                for (i, d) in sys.drives.iter().enumerate() {
                    let r = (ix, cy, ix + iw - 38, cy + 32);
                    button(&mut c, r, false, hover == Some(PHit::Drive(i)));
                    let state = if d.mountpoint.is_some() { "open" } else { "mount" };
                    let sw = self.text.width(state);
                    self.text.draw(&mut c, ix + 10, cy + 16, &self.text.fit(&format!("{} \u{b7} {}", d.label, d.size), iw - 38 - sw - 30), self.pal.on_surface);
                    self.text.draw(&mut c, ix + iw - 38 - sw - 10, cy + 16, state, muted);
                    p.hits.push((r, PHit::Drive(i)));
                    let e = (ix + iw - 32, cy, ix + iw, cy + 32);
                    button(&mut c, e, false, hover == Some(PHit::Eject(i)));
                    self.text.draw_centered(&mut c, ix + iw - 16, cy + 16, "\u{f01ef}", accent);
                    p.hits.push((e, PHit::Eject(i)));
                    cy += 38;
                }
            }
            Kind::TrayMenu => {
                if !p.menu_path.is_empty() {
                    let r = (ix, cy, ix + iw, cy + 30);
                    button(&mut c, r, false, hover == Some(PHit::MenuBack));
                    self.text.draw(&mut c, ix + 10, cy + 15, "\u{f0141}  Back", self.pal.on_surface);
                    p.hits.push((r, PHit::MenuBack));
                    cy += 34;
                }
                // a long menu shows its first lines (there is no scrolling yet)
                let level: Vec<crate::dbus::MenuEntry> = p.menu_level().iter().take(MENU_MAX_LINES).cloned().collect();
                for (i, e) in level.iter().enumerate() {
                    if e.separator {
                        c.rounded_rect(ix + 6, cy + 4, iw - 12, 1, 0.5, with_alpha(self.pal.on_surface, 0.15));
                        cy += 10;
                        continue;
                    }
                    let r = (ix, cy, ix + iw, cy + 30);
                    if e.enabled && hover == Some(PHit::MenuItem(i)) {
                        button(&mut c, r, false, true);
                    }
                    let indent = if e.check.is_some() { 22 } else { 0 };
                    let col = if e.enabled { self.pal.on_surface } else { muted };
                    if e.check == Some(true) {
                        self.text.draw(&mut c, ix + 10, cy + 15, "\u{f012c}", accent);
                    }
                    self.text.draw(&mut c, ix + 10 + indent, cy + 15, &self.text.fit(&e.label, iw - 20 - indent - 24), col);
                    if !e.children.is_empty() {
                        self.text.draw(&mut c, ix + iw - 24, cy + 15, "\u{f0142}", muted);
                    }
                    if e.enabled {
                        p.hits.push((r, PHit::MenuItem(i)));
                    }
                    cy += 32;
                }
            }
        }
        if let (Kind::Start, Some(side)) = (p.kind, p.side) {
            self.draw_side(&mut c, p, side, w as i32, hover);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{Battery, Volume};

    /// Writes a sheet of the four popups when VESPER_POPUP_OUT names a file (for looking at them by eye).
    #[test]
    fn write_sheet_if_asked() {
        let Some(path) = std::env::var_os("VESPER_POPUP_OUT") else { return };
        let mut r = Renderer::new("JetBrainsMono Nerd Font", 14.0, 32, 0);
        let mut sys = Sys { plugin: Default::default(), mic: Some(true), kb: None, media: None, tray: Vec::new(), stats: Some(crate::sys::Stats { cpu: 37, mem: 58, temp: Some(52) }), brightness: Some(60), notif: Default::default(), volume: Some(Volume { percent: 62, muted: false }), net: crate::sys::Net::Wifi, battery: Some(Battery { percent: 64, charging: false }), drives: Vec::new(), hist: Default::default() };
        let (w, h) = (2100u32, 880u32);
        sys.hist = crate::sys::Hist { cpu: (0..60).map(|i| 20 + (i * 7 % 35)).collect(), mem: (0..60).map(|i| 45 + (i % 6)).collect(), mem_used_kib: 7_600_000, mem_total_kib: 16_000_000 };
        let mut sheet = vec![0u8; (w * h * 4) as usize];
        for (i, kind) in [Kind::Start, Kind::Brightness, Kind::Network].into_iter().enumerate() {
            let mut p = Popup::open(kind, 0, &sys);
            p.anchor = 170;
            p.sinks = vec![Sink { name: "a".into(), label: "Built-in Audio Analog Stereo".into(), default: true }, Sink { name: "b".into(), label: "HDMI / DisplayPort".into(), default: false }];
            p.profiles = vec!["power-saver".into(), "balanced".into(), "performance".into()];
            p.profile = "balanced".into();
            p.battery_rows = vec![("Status".into(), "Discharging".into()), ("Time left".into(), "2h 14m".into())];
            p.side = Some(Side::Bluetooth);
            crate::bt::set_state_for_test(crate::bt::State {
                available: true,
                powered: true,
                scanning: true,
                devices: vec![
                    crate::bt::Device { path: "a".into(), name: "WH-1000XM4".into(), paired: true, connected: true, rssi: None, busy: None },
                    crate::bt::Device { path: "b".into(), name: "Keyboard".into(), paired: true, connected: false, rssi: None, busy: Some("connecting\u{2026}".into()) },
                    crate::bt::Device { path: "c".into(), name: "JBL Flip 6".into(), paired: false, connected: false, rssi: Some(-52), busy: None },
                    crate::bt::Device { path: "d".into(), name: "Galaxy Buds".into(), paired: false, connected: false, rssi: Some(-80), busy: None },
                ],
            });
            p.nets = vec![Wifi { ssid: "Home".into(), signal: 82, secure: true, active: true }, Wifi { ssid: "Cafe".into(), signal: 40, secure: false, active: false }];
            if kind == Kind::Network {
                if std::env::var("VESPER_POPUP_PROMPT").is_ok() { p.prompt = Some(Prompt { ssid: "Home-5G".into(), text: "hunter2".into(), error: Some("Wrong password, or the network could not be joined".into()), busy: false }); }
            }
            let (pw, ph) = (700u32, 880u32);
            if kind == Kind::Start {
                p.side = match std::env::var("VESPER_POPUP_SIDE").as_deref() {
                    Ok("mic") => Some(Side::Mic),
                    Ok("none") => None,
                    _ => Some(Side::Volume),
                };
                p.sources = vec![Sink { name: "m".into(), label: "Digital Microphone".into(), default: true }];
                p.ptt_key = "F4".into();
                p.vol = (60, false);
                p.sinks = vec![Sink { name: "1".into(), label: "Built-in Audio Analog Stereo".into(), default: true }, Sink { name: "2".into(), label: "HDMI / DisplayPort".into(), default: false }];
            }
            let mut buf = vec![0u8; (pw * ph * 4) as usize];
            r.draw_popup(&mut buf, pw, ph, &mut p, &sys, None);
            for y in 0..ph {
                for x in 0..pw {
                    let s = ((y * pw + x) * 4) as usize;
                    let d = ((y * w + x + i as u32 * pw) * 4) as usize;
                    sheet[d..d + 4].copy_from_slice(&buf[s..s + 4]);
                }
            }
        }
        sys.volume = None;
        // BGRA premultiplied -> RGBA over a dark background
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for px in sheet.chunks_exact(4) {
            let a = px[3] as u32;
            for c in [px[2], px[1], px[0]] {
                rgb.push(((c as u32) + (30 * (255 - a) / 255)) as u8);
            }
        }
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
}

#[cfg(test)]
mod wpctl_tests {
    use super::*;

    #[test]
    fn parses_wpctl_status_nodes() {
        // the shape of `wpctl status` output, trimmed
        let text = "PipeWire 'pipewire-0' [1.0.0]\n\nAudio\n ├─ Devices:\n │      45. Built-in Audio [alsa]\n │  \n ├─ Sinks:\n │  *   46. Built-in Audio Analog Stereo        [vol: 0.41]\n │      47. HDMI / DisplayPort                  [vol: 1.00]\n │  \n ├─ Sources:\n │  *   31. Built-in Microphone                 [vol: 0.82 MUTED]\n │  \n └─ Streams:\n\nVideo\n ├─ Sinks:\n │      99. Not audio\n";
        let nodes = |section: &str| -> Vec<(String, String, bool)> { parse_wpctl_nodes(text, section).into_iter().map(|n| (n.name, n.label, n.default)).collect() };
        let s = nodes("Sinks");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], ("46".into(), "Built-in Audio Analog Stereo".into(), true));
        assert_eq!(s[1].0, "47");
        assert_eq!(nodes("Sources"), vec![("31".to_string(), "Built-in Microphone".to_string(), true)]);
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
        // closing the menu with the Bluetooth panel open ends the search
        if self.side == Some(Side::Bluetooth) {
            crate::bt::send(crate::bt::Cmd::Scan(false));
        }
    }
}

/// SingularitySYS runs Wi-Fi itself (`netwatch`: wpa_supplicant and dhcpcd) and ships a `wifi` command for it. NetworkManager
/// does not own the adapter there, so asking it to connect fails: use that command instead.
pub fn uses_wifi_tool() -> bool {
    std::path::Path::new("/usr/bin/wifi").is_file() && std::path::Path::new("/usr/libexec/singularity/netwatch").is_file()
}

/// What the root `wifi` tool is handed ends up in wpa_supplicant's configuration file, so a network name or passphrase
/// with a line break or a quote could add settings of its own. Refuse those.
fn wifi_tool_input_ok(ssid: &str, pass: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
        return Err("The Wi-Fi tool cannot take this network name".into());
    }
    if !pass.is_empty() && (pass.len() < 8 || pass.len() > 63 || pass.chars().any(char::is_control)) {
        return Err("A Wi-Fi password is 8 to 63 characters".into());
    }
    Ok(())
}

/// Join `ssid` with the `wifi` command, as root through polkit (the Vesper password dialog asks for your password). An empty
/// `pass` joins an open network. Waits until it is connected or gave up, so run it off the main thread.
pub fn connect_with_wifi_tool(ssid: &str, pass: &str) -> Result<(), String> {
    wifi_tool_input_ok(ssid, pass)?;
    // The password goes in on standard input (the tool asks for it there when it is not on the command line): anything on
    // the command line can be read by every user in `ps`.
    use std::io::Write;
    let mut child = Command::new("pkexec")
        .args(["/usr/bin/wifi", "connect", ssid])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("pkexec is not installed ({e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{pass}\n").as_bytes()); // an empty line joins an open network
    }
    let out = child.wait_with_output().map_err(|e| format!("pkexec failed ({e})"))?;
    if out.status.success() {
        return Ok(());
    }
    // pkexec itself answers 126 (not authorised or dialog dismissed) and 127 (could not run it)
    if matches!(out.status.code(), Some(126 | 127)) {
        return Err("Not allowed: the password prompt was cancelled".into());
    }
    let text = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let last = text.lines().map(|l| l.trim().trim_start_matches('.').trim()).filter(|l| !l.is_empty()).last().unwrap_or("");
    Err(if last.is_empty() { "Could not connect: check the password".to_string() } else { last.chars().take(80).collect() })
}

/// [`connect_with_wifi_tool`], then say how it went in a notification (the popup may be gone by then).
pub fn connect_with_wifi_tool_and_tell(ssid: &str, pass: &str) -> Result<(), String> {
    let res = connect_with_wifi_tool(ssid, pass);
    match &res {
        Ok(()) => spawn("vesper-notify", &["send", "Wi-Fi", &format!("Connected to {ssid}")]),
        Err(e) => spawn("vesper-notify", &["send", "Wi-Fi", &format!("Could not connect to {ssid}: {e}")]),
    }
    res
}


/// Most lines of a tray menu that are shown.
const MENU_MAX_LINES: usize = 24;

impl Popup {
    /// The lines of the tray menu level that is open (the top level, or the submenu that `menu_path` leads into).
    pub fn menu_level(&self) -> &[crate::dbus::MenuEntry] {
        let Some(menu) = &self.menu else { return &[] };
        let mut level: &[crate::dbus::MenuEntry] = &menu.entries;
        for &i in &self.menu_path {
            match level.get(i) {
                Some(entry) => level = &entry.children,
                None => break,
            }
        }
        level
    }
}

impl Renderer {
    /// Height of the lines of the tray menu that is open.
    fn menu_height(p: &Popup) -> i32 {
        let back = if p.menu_path.is_empty() { 0 } else { 34 };
        let lines: i32 = p.menu_level().iter().take(MENU_MAX_LINES).map(|e| if e.separator { 10 } else { 32 }).sum();
        back + lines.max(32)
    }
}
/// Does NetworkManager already have a saved connection for this network?
pub fn is_saved(ssid: &str) -> bool {
    run("nmcli", &["-t", "-f", "NAME", "connection", "show"]).is_some_and(|s| s.lines().any(|l| l.trim() == ssid))
}

#[cfg(test)]
mod osd_tests {
    use super::*;

    /// Writes volume, boosted volume and brightness popups when VESPER_OSD_OUT names a file.
    #[test]
    fn write_osd_if_asked() {
        let Some(path) = std::env::var_os("VESPER_OSD_OUT") else { return };
        let mut r = Renderer::new("JetBrainsMono Nerd Font", 14.0, 32, 0);
        r.volume_max = 150;
        let (w, h) = (300u32, 64u32);
        let mut sheet = vec![0u8; (w * h * 3 * 4) as usize];
        for (i, (kind, pct, muted)) in [("volume", 62u32, false), ("volume", 128, false), ("brightness", 40, false)].into_iter().enumerate() {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            r.draw_osd(&mut buf, w, h, kind, pct, muted);
            let off = (h * w * 4) as usize * i;
            sheet[off..off + buf.len()].copy_from_slice(&buf);
        }
        let mut rgb = Vec::new();
        for px in sheet.chunks_exact(4) {
            let a = px[3] as u32;
            for c in [px[2], px[1], px[0]] {
                rgb.push((c as u32 + 30 * (255 - a) / 255) as u8);
            }
        }
        let mut enc = png::Encoder::new(std::fs::File::create(path).unwrap(), w, h * 3);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
}

#[cfg(test)]
mod stream_tests {
    use super::parse_wpctl_streams;

    #[test]
    fn lists_app_streams_not_their_ports() {
        let text = "Audio\n ├─ Sinks:\n │  *   55. Built-in Audio                   [vol: 0.40]\n │\n └─ Streams:\n        88. LibreWolf\n             92. output_FL       > Built-in Audio:playback_FL\t[active]\n             93. output_FR       > Built-in Audio:playback_FR\t[active]\n       101. Spotify\n            102. output_FL       > Built-in Audio:playback_FL\t[active]\n\nVideo\n └─ Streams:\n";
        assert_eq!(parse_wpctl_streams(text), vec![("88".to_string(), "LibreWolf".to_string()), ("101".to_string(), "Spotify".to_string())]);
    }
}

/// Ask NetworkManager to look for networks again (the list is read afterwards).
pub fn rescan_wifi() {
    let _ = crate::cmd::output(Command::new("nmcli").args(["device", "wifi", "rescan"]));
}

/// The networks in range, read now.
pub fn read_wifi_now() -> Result<Vec<Wifi>, String> {
    read_wifi()
}

#[cfg(test)]
mod wifi_tool_tests {
    use super::*;

    #[test]
    fn rejects_input_that_could_add_settings_to_wpa_supplicant() {
        assert!(wifi_tool_input_ok("Home", "correct horse").is_ok());
        assert!(wifi_tool_input_ok("Home", "").is_ok(), "open network");
        assert!(wifi_tool_input_ok("Ho\"me", "correct horse").is_err());
        assert!(wifi_tool_input_ok("Home\nssid=x", "correct horse").is_err());
        assert!(wifi_tool_input_ok("Home", "short").is_err());
        assert!(wifi_tool_input_ok("Home", "abcdefgh\nproxy_arp=1").is_err());
        assert!(wifi_tool_input_ok("", "correct horse").is_err());
        assert!(wifi_tool_input_ok(&"x".repeat(33), "correct horse").is_err());
    }
}

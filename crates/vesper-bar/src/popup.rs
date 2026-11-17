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
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slider {
    Volume,
    Brightness,
    /// Night-light strength in the side panel.
    Night,
    /// One app's own volume in the mixer (index into Popup.apps).
    App(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StartAct {
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
    pub battery_note: String,
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
            battery_note: String::new(),
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
        };
        match kind {
            Kind::Volume => {
                p.sinks = read_sinks();
                p.apps = read_apps();
                p.quick = crate::quick::read(sys);
            }
            Kind::Battery => {
                p.battery_note = battery_note();
                (p.profiles, p.profile) = read_profiles();
            }
            Kind::Network => match read_wifi() {
                Ok(n) => p.nets = n,
                Err(e) => p.nets_error = Some(e),
            },
            Kind::Start => p.quick = crate::quick::read(sys),
            Kind::Brightness => {
                p.quick = crate::quick::read(sys);
                p.night_strength = vesper_config::Config::load().appearance.night_light_strength.clamp(1, 100);
            }
            Kind::Calendar | Kind::Drives => {}
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
            Side::Sound => self.sinks = read_sinks(),
            Side::Mic => {
                self.sources = read_sources();
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
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a command without waiting for it on the UI thread.
/// A screen recording is running (see scripts/vesper-record).
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


fn read_profiles() -> (Vec<String>, String) {
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

fn battery_note() -> String {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return String::new() };
    let Some(bat) = dir.flatten().find(|e| e.file_name().to_string_lossy().starts_with("BAT")) else { return String::new() };
    let rd = |n: &str| std::fs::read_to_string(bat.path().join(n)).ok().and_then(|s| s.trim().parse::<f64>().ok());
    let status = std::fs::read_to_string(bat.path().join("status")).unwrap_or_default();
    let status = status.trim();
    // energy_* (µWh) with power_now (µW), or charge_* (µAh) with current_now (µA): both give hours
    let (now, full, rate) = match (rd("energy_now"), rd("energy_full"), rd("power_now")) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => (rd("charge_now").unwrap_or(0.0), rd("charge_full").unwrap_or(0.0), rd("current_now").unwrap_or(0.0)),
    };
    if rate <= 0.0 {
        return String::new();
    }
    let hours = match status {
        "Discharging" => now / rate,
        "Charging" => (full - now).max(0.0) / rate,
        _ => return String::new(),
    };
    let mins = (hours * 60.0).round() as u32;
    let t = if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins}m") };
    if status == "Discharging" { format!("{t} left") } else { format!("{t} until full") }
}

fn read_wifi() -> Result<Vec<Wifi>, String> {
    let out = Command::new("nmcli")
        .args(["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "no"])
        .output()
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
    nets.truncate(8);
    Ok(nets)
}

fn month_start(offset: i32) -> NaiveDate {
    let today = Local::now().date_naive();
    let total = today.year() * 12 + today.month0() as i32 + offset;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap_or(today)
}

impl Renderer {
    /// The brightness and volume sliders and the toggle tiles of the quick settings. Returns the height used.
    #[allow(clippy::too_many_arguments)]
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
        let pw = 300;
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
        } else if side == Side::Night {
            14 + 66 + 10
        } else if side == Side::Brightness {
            14 + 34 + 66 + 10
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
        if side == Side::Night {
            self.slider_row(c, ix, iw, cy, "Night light strength", p.night_strength, false, Slider::Night, &mut p.hits);
            return;
        }
        if side == Side::Brightness {
            let on = p.quick.night;
            let tile = PHit::Tile(crate::quick::Tile::NightLight);
            row(c, &mut p.hits, cy, on, hover == Some(tile), "Night light", if on { "on" } else { "off" }, tile);
            cy += 34;
            self.slider_row(c, ix, iw, cy, "Night light strength", p.night_strength, false, Slider::Night, &mut p.hits);
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

    /// Paint the box of a popup the way the bar is painted: its corner radius, how see-through it is, and its border
    /// (colour, width, blend).
    fn panel(&self, c: &mut Canvas, x: i32, y: i32, w: i32, h: i32) {
        let mut border = self.style.border.unwrap_or(self.pal.primary);
        if self.style.border_blend {
            border[3] = (border[3] as u32 * self.bg_alpha as u32 / 255) as u8;
        }
        let fill = vesper_config::theme::alpha(self.pal.surface, self.bg_alpha);
        let r = self.style.radius.clamp(8, 28);
        shape::paint(c, &Shape::Round { x, y, w, h, r }, fill, self.style.border_width.max(1), border);
    }

    /// A labelled slider row at (ix, y): the label on the left, the percentage on the right, the track below.
    /// Records the grab area and returns the height it used.
    #[allow(clippy::too_many_arguments)]
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
        let mut c = Canvas::new(canvas, w, h);
        c.clear([0, 0, 0, 0]);
        let accent = self.pal.primary;
        let (bw, bh) = match p.kind {
            Kind::Brightness => (320, self.side_height(p, Side::Brightness) + 66),
            Kind::Start => (370, 14 + 46 + 94 + (66 * 2 + 8 + 68 * 2 + 8) + 16 + 24 + 34 * if sys.stats.and_then(|s| s.temp).is_some() { 3 } else { 2 } + 8 + 58 + 10),
            Kind::Calendar => {
                let first = month_start(p.month_offset);
                let lead = first.weekday().num_days_from_monday() as i32;
                let days = (month_start(p.month_offset + 1) - first).num_days() as i32;
                let rows = (lead + days + 6) / 7;
                (288, 14 + 38 + 26 + rows * 34 + 10)
            }
            Kind::Volume => (320, self.side_height(p, Side::Volume) - 32 + 66),
            Kind::Battery => (290, 14 + 64 + 28 + if p.profiles.is_empty() { 0 } else { 30 + 38 } + 10),
            Kind::Network => (320, self.side_height(p, Side::Wifi)),
            Kind::Drives => (320, 14 + 32 + 38 * sys.drives.len().max(1) as i32 + 10),
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
                let os = vesper_apps::distro::os_release();
                let name = os.get("NAME").or_else(|| os.get("PRETTY_NAME")).cloned().unwrap_or_else(|| "Start".into());
                // tools along the top, right to left: settings, wallpapers, screenshot, copy text, read QR code
                let tools = [
                    (StartAct::Settings, "Settings", "\u{f0493}"),
                    (StartAct::Wallpapers, "Wallpapers", "\u{f02e9}"),
                    (StartAct::Screenshot, "Screenshot", "\u{f0100}"),
                    (StartAct::CopyText, "Copy text from the screen", "\u{f113d}"),
                    (StartAct::ReadQr, "Read a QR code", "\u{f0432}"),
                    (StartAct::Record, "Record the screen", "\u{f044a}"),
                ];
                let mut label = name.clone();
                let mut bx = ix + iw;
                let mut left_edge = bx;
                for (act, tip, glyph) in tools {
                    bx -= 34;
                    let r = (bx, cy, bx + 34, cy + 34);
                    let hot = hover == Some(PHit::Start(act));
                    button(&mut c, r, act == StartAct::Record && recording(), hot);
                    self.text.draw_centered(&mut c, bx + 17, cy + 17, glyph, if act == StartAct::Record && recording() { self.pal.on_primary } else { accent });
                    p.hits.push((r, PHit::Start(act)));
                    if hot {
                        label = tip.to_string();
                    }
                    left_edge = bx;
                    bx -= 6;
                }
                let shown = self.text.fit(&label, (left_edge - ix - 10).max(40));
                self.text.draw(&mut c, ix, cy + 17, &shown, if label == name { accent } else { self.pal.on_surface });
                cy += 46;
                let half = (iw - 8) / 2;
                for (i, (act, label, glyph)) in [(StartAct::Apps, "Apps", "\u{f0570}"), (StartAct::Files, "Files", "\u{f024b}"), (StartAct::Clipboard, "Clipboard", "\u{f0ea}"), (StartAct::Emoji, "Emoji", "\u{f118}")].into_iter().enumerate() {
                    let (col, row) = (i as i32 % 2, i as i32 / 2);
                    let bx = ix + col * (half + 8);
                    let by = cy + row * 44;
                    let r = (bx, by, bx + half, by + 36);
                    button(&mut c, r, false, hover == Some(PHit::Start(act)));
                    self.text.draw_centered(&mut c, bx + half / 2 - self.text.width(label) / 2 - 4, by + 18, glyph, accent);
                    self.text.draw_centered(&mut c, bx + half / 2 + 14, by + 18, label, self.pal.on_surface);
                    p.hits.push((r, PHit::Start(act)));
                }
                cy += 94;
                cy += self.quick_block(&mut c, ix, iw, cy, p.quick, sys, hover, p.side, &mut p.hits) + 16;
                self.text.draw(&mut c, ix, cy + 6, "System", muted);
                cy += 24;
                let rows: Vec<(&str, String, u32)> = match sys.stats {
                    Some(s) => {
                        let mut v = vec![("CPU", format!("{}%", s.cpu), s.cpu), ("Memory", format!("{}%", s.mem), s.mem)];
                        if let Some(t) = s.temp {
                            v.push(("Temperature", format!("{t}\u{b0}C"), t.min(100)));
                        }
                        v
                    }
                    None => vec![("CPU", "\u{2026}".into(), 0), ("Memory", "\u{2026}".into(), 0)],
                };
                for (label, value, pct) in &rows {
                    self.text.draw(&mut c, ix, cy + 10, label, self.pal.on_surface);
                    self.text.draw(&mut c, ix + iw - self.text.width(value), cy + 10, value, accent);
                    c.rounded_rect(ix, cy + 22, iw, 4, 2.0, with_alpha(accent, 0.22));
                    let f = (iw as f32 * (*pct).min(100) as f32 / 100.0) as i32;
                    if f > 0 {
                        c.rounded_rect(ix, cy + 22, f.max(4), 4, 2.0, accent);
                    }
                    cy += 34;
                }
                cy += 8;
                let acts = [
                    (StartAct::Lock, "Lock", "\u{f033e}"),
                    (StartAct::Sleep, "Sleep", "\u{f0904}"),
                    (StartAct::Logout, "Log out", "\u{f0343}"),
                    (StartAct::Restart, "Restart", "\u{f0709}"),
                    (StartAct::Shutdown, "Shut down", "\u{f0425}"),
                ];
                let n = acts.len() as i32;
                let bw2 = (iw - (n - 1) * 6) / n;
                for (i, (act, label, glyph)) in acts.into_iter().enumerate() {
                    let bx = ix + i as i32 * (bw2 + 6);
                    let r = (bx, cy, bx + bw2, cy + 58);
                    let confirm = p.pending == Some(act);
                    let hot = hover == Some(PHit::Start(act));
                    let fill = if confirm { self.pal.error } else if hot { with_alpha(self.pal.on_surface, 0.16) } else { with_alpha(self.pal.on_surface, 0.08) };
                    c.rounded_rect(r.0, r.1, r.2 - r.0, r.3 - r.1, 9.0, fill);
                    let fg = if confirm { self.pal.on_primary } else { accent };
                    self.text.draw_centered(&mut c, bx + bw2 / 2, cy + 20, glyph, fg);
                    let small = self.text.resized(11.0);
                    small.draw_centered(&mut c, bx + bw2 / 2, cy + 43, if confirm { "Sure?" } else { label }, if confirm { self.pal.on_primary } else { self.pal.on_surface });
                    p.hits.push((r, PHit::Start(act)));
                }
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
            Kind::Brightness => {
                match sys.brightness {
                    Some(b) => {
                        self.slider_row(&mut c, ix, iw, cy, "Brightness", b, false, Slider::Brightness, &mut p.hits);
                    }
                    None => {
                        self.text.draw(&mut c, ix, cy + 12, "Brightness", accent);
                        self.text.draw(&mut c, ix, cy + 40, &self.text.fit("No adjustable screen backlight", iw), muted);
                    }
                }
                // the same night-light controls as the start menu's brightness panel
                self.draw_side_body(&mut c, p, Side::Brightness, x, y + 66, bw, hover, false);
            }
            Kind::Volume => {
                let (percent, mutedv) = sys.volume.map_or((0, false), |v| (v.percent, v.muted));
                self.slider_row(&mut c, ix, iw, cy, "Volume", percent, mutedv, Slider::Volume, &mut p.hits);
                // the same mute, boost, outputs and per-app mixer as the start menu's volume panel
                self.draw_side_body(&mut c, p, Side::Volume, x, y + 66, bw, hover, false);
            }
            Kind::Battery => {
                match sys.battery {
                    Some(b) => {
                        let big = self.text.resized(30.0);
                        big.draw(&mut c, ix, cy + 22, &format!("{}%", b.percent), accent);
                        let state = if b.charging { "Charging" } else { "On battery" };
                        self.text.draw(&mut c, ix, cy + 52, state, self.pal.on_surface);
                        cy += 64;
                    }
                    None => {
                        self.text.draw(&mut c, ix, cy + 22, "No battery", accent);
                        cy += 64;
                    }
                }
                if !p.battery_note.is_empty() {
                    self.text.draw(&mut c, ix, cy + 8, &p.battery_note, muted);
                }
                cy += 28;
                if !p.profiles.is_empty() {
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
        let mut sys = Sys { plugin: Default::default(), mic: Some(true), kb: None, media: None, tray: Vec::new(), stats: Some(crate::sys::Stats { cpu: 37, mem: 58, temp: Some(52) }), brightness: Some(60), notif: Default::default(), volume: Some(Volume { percent: 62, muted: false }), net: crate::sys::Net::Wifi, battery: Some(Battery { percent: 64, charging: false }), drives: Vec::new() };
        let (w, h) = (2100u32, 700u32);
        let mut sheet = vec![0u8; (w * h * 4) as usize];
        for (i, kind) in [Kind::Start, Kind::Brightness, Kind::Network].into_iter().enumerate() {
            let mut p = Popup::open(kind, 0, &sys);
            p.anchor = 170;
            p.sinks = vec![Sink { name: "a".into(), label: "Built-in Audio Analog Stereo".into(), default: true }, Sink { name: "b".into(), label: "HDMI / DisplayPort".into(), default: false }];
            p.profiles = vec!["power-saver".into(), "balanced".into(), "performance".into()];
            p.profile = "balanced".into();
            p.battery_note = "2h 14m left".into();
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
                p.prompt = Some(Prompt { ssid: "Home-5G".into(), text: "hunter2".into(), error: Some("Wrong password, or the network could not be joined".into()), busy: false });
            }
            let (pw, ph) = (700u32, 700u32);
            if kind == Kind::Start {
                p.side = Some(Side::Volume);
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

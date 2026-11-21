//! vesper-shelltint-host: Vesper's built-in helper for the ShellTint browser extension
//! (https://github.com/Jazzmedo/ShellTint).
//!
//! The browser starts this program through a native messaging manifest named `shelltint` (installed system-wide with
//! Vesper) and talks to it over stdin/stdout: each message is a 4-byte native-endian length and UTF-8 JSON.
//! It speaks protocol 1 of the extension's own helper, so the extension works without installing that helper.
//!
//! What it does: sends Vesper's palette (the colours matugen made from the wallpaper), and sends it again when the
//! colours change. The browser's toolbar and tabs then follow the wallpaper. What it does not do: the website styling
//! (Catppuccin userstyles) needs the extension's separate build step, so it reports no styles.
//!
//! The helper is off unless `shelltint = true` in `~/.config/vesper/theme.toml` (Settings > Colours from the wallpaper).
//! While off it stays connected and tells the extension so, and starts answering as soon as the setting is switched on.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use vesper_config::theme::{colors_file, Palette, ThemeSettings};

const PROTOCOL: u64 = 1;
/// The browser drops the connection when a host sends a message over 1 MB.
const MAX_OUT: usize = 1024 * 1024;
const MAX_IN: usize = 8 * 1024 * 1024;
const TICK: Duration = Duration::from_secs(1);

fn read_message(input: &mut impl Read) -> Option<Value> {
    let mut head = [0u8; 4];
    input.read_exact(&mut head).ok()?;
    let len = u32::from_ne_bytes(head) as usize;
    if len > MAX_IN {
        return None; // the stream cannot be resynchronised
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body).ok()?;
    // an unreadable message is skipped, not fatal
    Some(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Writes one message. False when the browser has gone away.
fn send(message: &Value) -> bool {
    let body = match serde_json::to_vec(message) {
        Ok(b) => b,
        Err(_) => return true,
    };
    if body.len() > MAX_OUT {
        eprintln!("vesper-shelltint: a message of {} bytes is over the 1 MB limit; not sent", body.len());
        return true;
    }
    let mut out = std::io::stdout().lock();
    out.write_all(&(body.len() as u32).to_ne_bytes()).and_then(|_| out.write_all(&body)).and_then(|_| out.flush()).is_ok()
}

fn hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Relative lightness 0..1 (enough to tell a dark surface from a light one).
fn lightness(c: [u8; 4]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

fn mtime(path: &std::path::Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64())
}

struct Host {
    /// The extension's "Light or dark" choice: follow, dark or light.
    mode_setting: String,
    /// The last `ST_PALETTE` sent, to send a new one only when something changed.
    last_palette: Option<String>,
}

impl Host {
    fn enabled() -> bool {
        ThemeSettings::load().shelltint
    }

    fn palette_message(&self) -> Value {
        if !Self::enabled() {
            return json!({
                "type": "ST_PALETTE",
                "error": "Vesper's built-in ShellTint helper is switched off: turn it on in Vesper Settings, Colours from the wallpaper",
                "lastGood": null
            });
        }
        let p = Palette::load();
        let theme_mode = ThemeSettings::load().mode;
        let (mode, source) = match self.mode_setting.as_str() {
            "dark" | "light" => (self.mode_setting.clone(), "forced"),
            _ => match theme_mode.as_str() {
                "dark" | "light" => (theme_mode.clone(), "shell"),
                _ => ((if lightness(p.surface) > 0.5 { "light" } else { "dark" }).to_string(), "inferred"),
            },
        };
        let roles = json!({
            "primary": hex(p.primary),
            "on_primary": hex(p.on_primary),
            "primary_container": hex(p.primary_container),
            "on_primary_container": hex(p.on_primary_container),
            "secondary": hex(p.secondary),
            "tertiary": hex(p.tertiary),
            "error": hex(p.error),
            "surface": hex(p.surface),
            "background": hex(p.surface),
            "on_surface": hex(p.on_surface),
            "on_background": hex(p.on_surface),
            "on_surface_variant": hex(p.on_surface_variant),
            "surface_container": hex(p.surface_container),
            "surface_container_high": hex(p.surface_container_high),
            "surface_container_highest": hex(p.surface_container_highest),
            "outline": hex(p.outline),
            "outline_variant": hex(p.outline_variant),
            "scrim": hex(p.scrim),
        });
        let mut h = DefaultHasher::new();
        roles.to_string().hash(&mut h);
        mode.hash(&mut h);
        let path = colors_file();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        json!({
            "type": "ST_PALETTE",
            "palette": {
                "format": 1,
                "hash": format!("{:016x}", h.finish()),
                "generatedAt": now,
                "source": { "id": "vesper", "label": "Vesper", "path": path.to_string_lossy(), "mtime": mtime(&path) },
                "mode": mode,
                "modeSource": source,
                "roles": roles,
                "extras": {},
                "warnings": []
            }
        })
    }

    /// Send the palette if it differs from the last one sent (or `force`).
    fn send_palette(&mut self, force: bool) -> bool {
        let msg = self.palette_message();
        // `generatedAt` changes on every call, so compare without it
        let key = match msg.get("palette") {
            Some(p) => format!("{}|{}", p["hash"], p["source"]["mtime"]),
            None => msg.to_string(),
        };
        if !force && self.last_palette.as_deref() == Some(&key) {
            return true;
        }
        self.last_palette = Some(key);
        send(&msg)
    }

    fn hello(&self) -> bool {
        let dir = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
        send(&json!({
            "type": "ST_HELLO",
            "hostVersion": format!("vesper-{}", env!("CARGO_PKG_VERSION")),
            "protocol": PROTOCOL,
            "settingsMirror": null,
            "paths": {
                "install": "/usr/bin/vesper-shelltint-host",
                "cache": dir(colors_file().parent().map(|p| p.to_path_buf()).unwrap_or_default()),
                "config": dir(ThemeSettings::file()),
                "log": ""
            }
        }))
    }

    fn handle(&mut self, message: &Value) -> bool {
        let req_id = message.get("reqId").cloned().unwrap_or(Value::Null);
        match message.get("type").and_then(Value::as_str) {
            Some("ST_SETTINGS") => {
                let mode = message["settings"]["mode"].as_str().unwrap_or("follow").to_string();
                let changed = mode != self.mode_setting;
                self.mode_setting = mode;
                if changed {
                    return self.send_palette(false);
                }
                true
            }
            Some("ST_SYNC") => {
                self.send_palette(true)
                    && send(&json!({ "type": "ST_STYLES_INDEX", "index": null }))
                    && send(&json!({ "type": "ST_STATUS", "building": false, "lastBuild": null, "lastUpdateCheck": null }))
            }
            Some("ST_GET_SITE_CSS") => {
                // no website styles in the built-in helper: every block is reported missing
                let hashes = message.get("hashes").cloned().unwrap_or(json!([]));
                send(&json!({ "type": "ST_SITE_CSS", "reqId": req_id, "items": [], "final": true, "missing": hashes }))
            }
            Some("ST_DETECT_SOURCES") => {
                let path = colors_file();
                send(&json!({
                    "type": "ST_SOURCES",
                    "reqId": req_id,
                    "active": "vesper",
                    "sources": [{ "id": "vesper", "label": "Vesper", "found": path.exists(), "path": path.to_string_lossy(), "mtime": mtime(&path), "ok": Self::enabled() }]
                }))
            }
            Some("ST_REBUILD") => self.send_palette(true) && send(&json!({ "type": "ST_ACTION_RESULT", "reqId": req_id, "ok": true })),
            Some("ST_CHECK_UPDATES") => send(&json!({
                "type": "ST_ACTION_RESULT",
                "reqId": req_id,
                "ok": false,
                "error": "Vesper's built-in helper colours the browser only; website styles need the separate ShellTint helper"
            })),
            Some(other) => send(&json!({ "type": "ST_ERROR", "reason": "unknown-message", "detail": other.chars().take(100).collect::<String>() })),
            None => true,
        }
    }
}

fn main() {
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        while let Some(msg) = read_message(&mut stdin) {
            if tx.send(msg).is_err() {
                break;
            }
        }
        // dropping `tx` tells the main loop the browser closed the pipe
    });

    let mut host = Host { mode_setting: "follow".into(), last_palette: None };
    if !host.hello() {
        return;
    }
    loop {
        let alive = match rx.recv_timeout(TICK) {
            Ok(msg) => host.handle(&msg),
            // colours or the on/off setting may have changed
            Err(RecvTimeoutError::Timeout) => host.send_palette(false),
            Err(RecvTimeoutError::Disconnected) => false,
        };
        if !alive {
            break;
        }
    }
}

//! D-Bus parts of the bar, each on its own thread: MPRIS media players and the StatusNotifier tray.
//! They send what changed to the UI thread over a channel; clicks go back as one-off calls.

use std::process::Command;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::{
    blocking::{fdo::DBusProxy, Connection, Proxy},
    zvariant::{OwnedValue, Value},
};

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// Bus name of the player.
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrayItem {
    pub bus: String,
    pub path: String,
    pub title: String,
    pub icon_name: String,
    pub theme_path: String,
    /// Largest pixmap the app offered, as straight RGBA.
    pub pixmap: Option<(i32, i32, Vec<u8>)>,
}

pub enum Dbus {
    /// The Bluetooth picture changed (see `bt`).
    Bt,
    Media(Option<Media>),
    Tray(Vec<TrayItem>),
    /// The menu of tray icon number `index` (clicked at `x`) arrived, or it has none (`None`: use the app's own menu).
    TrayMenu { x: f64, index: usize, menu: Option<TrayMenu> },
    /// The result of joining a Wi-Fi network with a typed password.
    Wifi(Result<(), String>),
    /// A plugin widget's text (empty = hide it).
    Plugin(&'static str, String),
}

// ---------------------------------------------------------------------------------------------------- media

pub fn start_media(tx: Sender<Dbus>) {
    std::thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let Ok(bus) = DBusProxy::new(&conn) else { return };
        let mut last: Option<Option<Media>> = None;
        loop {
            let now = current_player(&conn, &bus);
            if last.as_ref() != Some(&now) {
                if tx.send(Dbus::Media(now.clone())).is_err() {
                    return;
                }
                last = Some(now);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn current_player(conn: &Connection, bus: &DBusProxy) -> Option<Media> {
    let names = bus.list_names().ok()?;
    let mut best: Option<Media> = None;
    for n in names.iter().filter(|n| n.as_str().starts_with("org.mpris.MediaPlayer2.")) {
        let Ok(p) = Proxy::new(conn, n.as_str().to_owned(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player") else { continue };
        let Ok(status) = p.get_property::<String>("PlaybackStatus") else { continue };
        if status == "Stopped" {
            continue;
        }
        let meta: HashMap<String, OwnedValue> = p.get_property("Metadata").unwrap_or_default();
        let title = meta.get("xesam:title").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
        let artist = meta.get("xesam:artist").and_then(|v| Vec::<String>::try_from(v.clone()).ok()).map(|a| a.join(", ")).unwrap_or_default();
        let m = Media { player: n.as_str().to_owned(), title, artist, playing: status == "Playing" };
        if m.playing {
            return Some(m);
        }
        best.get_or_insert(m);
    }
    best
}

/// "PlayPause", "Next" or "Previous" on `player`.
pub fn media_call(player: String, method: &'static str) {
    std::thread::spawn(move || {
        if let Ok(conn) = Connection::session() {
            if let Ok(p) = Proxy::new(&conn, player, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player") {
                let _: Result<(), _> = p.call(method, &());
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------------- tray

struct Watcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: String, #[zbus(header)] hdr: zbus::message::Header<'_>) {
        let sender = hdr.sender().map(|s| s.to_string()).unwrap_or_default();
        // an app passes either its object path (the bus name is then the sender) or a bus name
        let id = if service.starts_with('/') { format!("{sender}{service}") } else { format!("{service}/StatusNotifierItem") };
        let mut items = self.items.lock().unwrap();
        if !items.contains(&id) {
            items.push(id);
        }
    }

    fn register_status_notifier_host(&self, _service: String) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

pub fn start_tray(tx: Sender<Dbus>) {
    std::thread::spawn(move || {
        let items: Arc<Mutex<Vec<String>>> = Arc::default();
        // be the watcher if nobody else is; otherwise just read the existing one
        let own = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.name("org.kde.StatusNotifierWatcher"))
            .and_then(|b| b.serve_at("/StatusNotifierWatcher", Watcher { items: items.clone() }))
            .and_then(|b| b.build());
        let (conn, remote) = match own {
            Ok(c) => (c, false),
            Err(_) => {
                let Ok(c) = Connection::session() else { return };
                let _ = c.request_name(format!("org.kde.StatusNotifierHost-{}", std::process::id()));
                if let Ok(w) = Proxy::new(&c, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher") {
                    let _: Result<(), _> = w.call("RegisterStatusNotifierHost", &(format!("org.kde.StatusNotifierHost-{}", std::process::id()),));
                }
                (c, true)
            }
        };
        let Ok(bus) = DBusProxy::new(&conn) else { return };
        let mut last: Option<Vec<TrayItem>> = None;
        loop {
            let ids: Vec<String> = if remote {
                Proxy::new(&conn, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher")
                    .and_then(|w| w.get_property::<Vec<String>>("RegisteredStatusNotifierItems"))
                    .unwrap_or_default()
            } else {
                // forget apps that have left the bus
                let mut list = items.lock().unwrap();
                list.retain(|id| {
                    let bus_name = id.split('/').next().unwrap_or("");
                    zbus::names::BusName::try_from(bus_name).ok().is_some_and(|n| bus.name_has_owner(n).unwrap_or(false))
                });
                list.clone()
            };
            let now: Vec<TrayItem> = ids.iter().filter_map(|id| read_item(&conn, id)).collect();
            if last.as_ref() != Some(&now) {
                if tx.send(Dbus::Tray(now.clone())).is_err() {
                    return;
                }
                last = Some(now);
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
    });
}

fn read_item(conn: &Connection, id: &str) -> Option<TrayItem> {
    let (bus, rest) = id.split_once('/')?;
    let path = format!("/{rest}");
    let p = Proxy::new(conn, bus.to_owned(), path.clone(), "org.kde.StatusNotifierItem").ok()?;
    let status = p.get_property::<String>("Status").unwrap_or_default();
    if status == "Passive" {
        return None;
    }
    let pixmap = p.get_property::<Vec<(i32, i32, Vec<u8>)>>("IconPixmap").ok().and_then(|v| {
        let (w, h, argb) = v.into_iter().max_by_key(|(w, h, _)| w * h)?;
        if w <= 0 || h <= 0 || argb.len() < (w * h * 4) as usize {
            return None;
        }
        // ARGB32 in network byte order -> RGBA
        let mut rgba = Vec::with_capacity(argb.len());
        for px in argb.chunks_exact(4) {
            rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
        }
        Some((w, h, rgba))
    });
    Some(TrayItem {
        bus: bus.to_owned(),
        path,
        title: p.get_property::<String>("Title").unwrap_or_default(),
        icon_name: p.get_property::<String>("IconName").unwrap_or_default(),
        theme_path: p.get_property::<String>("IconThemePath").unwrap_or_default(),
        pixmap,
    })
}

// ---------------------------------------------------------------------------------------------------- tray menu

/// One line of a tray icon's menu (`com.canonical.dbusmenu`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// A check or radio item: whether it is ticked.
    pub check: Option<bool>,
    pub children: Vec<MenuEntry>,
}

/// A tray icon's menu and where to send a click on one of its lines.
#[derive(Debug, Clone, PartialEq)]
pub struct TrayMenu {
    pub bus: String,
    pub menu_path: String,
    pub entries: Vec<MenuEntry>,
}

fn unwrap_variant<'a, 'b>(mut v: &'b Value<'a>) -> &'b Value<'a> {
    while let Value::Value(inner) = v {
        v = inner;
    }
    v
}

/// `Save _As` -> `Save As` (the underscore marks a keyboard shortcut letter; `__` is a real underscore).
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::new();
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One node of `GetLayout`: `(id, properties, children)`, each child wrapped in a variant. None for a hidden line.
fn parse_menu_node(v: &Value<'_>) -> Option<MenuEntry> {
    let Value::Structure(s) = unwrap_variant(v) else { return None };
    let fields = s.fields();
    let Value::I32(id) = unwrap_variant(fields.first()?) else { return None };
    let mut entry = MenuEntry { id: *id, enabled: true, ..Default::default() };
    let mut visible = true;
    if let Value::Dict(props) = unwrap_variant(fields.get(1)?) {
        for (key, val) in props.iter() {
            let Value::Str(key) = unwrap_variant(key) else { continue };
            match (key.as_str(), unwrap_variant(val)) {
                ("label", Value::Str(s)) => entry.label = strip_mnemonic(s.as_str()),
                ("enabled", Value::Bool(b)) => entry.enabled = *b,
                ("visible", Value::Bool(b)) => visible = *b,
                ("type", Value::Str(s)) => entry.separator = s.as_str() == "separator",
                ("toggle-type", Value::Str(s)) if !s.as_str().is_empty() => entry.check = entry.check.or(Some(false)),
                ("toggle-state", Value::I32(n)) => entry.check = Some(*n == 1),
                _ => {}
            }
        }
    }
    if !visible {
        return None;
    }
    if let Some(Value::Array(children)) = fields.get(2).map(unwrap_variant) {
        entry.children = children.iter().filter_map(parse_menu_node).collect();
    }
    Some(entry)
}

/// Ask a tray item for its menu. None if it has none (or does not answer): the caller then falls back to the app's own menu.
/// Blocks on the app, so call it off the UI thread.
pub fn read_tray_menu(item: &TrayItem) -> Option<TrayMenu> {
    let conn = Connection::session().ok()?;
    let sni = Proxy::new(&conn, item.bus.clone(), item.path.clone(), "org.kde.StatusNotifierItem").ok()?;
    let menu_path = sni.get_property::<zbus::zvariant::OwnedObjectPath>("Menu").ok()?.as_str().to_owned();
    if menu_path.len() < 2 {
        return None; // "/" means no menu
    }
    let menu = Proxy::new(&conn, item.bus.clone(), menu_path.clone(), "com.canonical.dbusmenu").ok()?;
    let _: Result<bool, _> = menu.call("AboutToShow", &(0i32,));
    let (_revision, layout): (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) = menu.call("GetLayout", &(0i32, -1i32, Vec::<String>::new())).ok()?;
    let entries: Vec<MenuEntry> = layout.2.iter().filter_map(|v| parse_menu_node(v)).collect();
    (!entries.is_empty()).then_some(TrayMenu { bus: item.bus.clone(), menu_path, entries })
}

/// Tell the app that line `id` of its menu was clicked.
pub fn tray_menu_click(bus: String, menu_path: String, id: i32) {
    std::thread::spawn(move || {
        if let Ok(conn) = Connection::session() {
            if let Ok(menu) = Proxy::new(&conn, bus, menu_path, "com.canonical.dbusmenu") {
                let _: Result<(), _> = menu.call("Event", &(id, "clicked", Value::from(0i32), 0u32));
            }
        }
    });
}

/// "Activate", "ContextMenu" or "SecondaryActivate" on a tray item, at screen position (x, y).
pub fn tray_call(item: &TrayItem, method: &'static str, x: i32, y: i32) {
    let (bus, path) = (item.bus.clone(), item.path.clone());
    std::thread::spawn(move || {
        if let Ok(conn) = Connection::session() {
            if let Ok(p) = Proxy::new(&conn, bus, path, "org.kde.StatusNotifierItem") {
                let _: Result<(), _> = p.call(method, &(x, y));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::channel;

    /// Needs a session bus; skipped without one or if another tray watcher is already running.
    #[test]
    fn watcher_accepts_items() {
        let Ok(conn) = Connection::session() else { return };
        if conn.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "NameHasOwner", &("org.kde.StatusNotifierWatcher",)).ok().and_then(|m| m.body().deserialize::<bool>().ok()) == Some(true) {
            return;
        }
        let (tx, _rx) = channel::channel::<Dbus>();
        start_tray(tx);
        std::thread::sleep(Duration::from_millis(500));
        let p = Proxy::new(&conn, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher").unwrap();
        let _: () = p.call("RegisterStatusNotifierItem", &("/StatusNotifierItem",)).unwrap();
        let items: Vec<String> = p.get_property("RegisteredStatusNotifierItems").unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].ends_with("/StatusNotifierItem"));
        assert!(p.get_property::<bool>("IsStatusNotifierHostRegistered").unwrap());
    }
}

// ---------------------------------------------------------------------------------------------------- backlight

/// Set the screen brightness (1 to 100 percent). A worker thread applies the newest value, through systemd-logind
/// (no root needed for your own session), or `brightnessctl` if that is not available.
pub fn set_backlight(percent: u32) {
    use std::sync::{mpsc, OnceLock};
    static TX: OnceLock<mpsc::Sender<u32>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<u32>();
        std::thread::spawn(move || {
            let conn = Connection::system().ok();
            while let Ok(mut p) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    p = newer; // dragging sends many values: only the last matters
                }
                apply_backlight(conn.as_ref(), p);
            }
        });
        tx
    });
    let _ = tx.send(percent.clamp(1, 100));
}

fn apply_backlight(conn: Option<&Connection>, percent: u32) {
    let device = std::fs::read_dir("/sys/class/backlight").ok().and_then(|d| {
        let mut v: Vec<_> = d.flatten().collect();
        v.sort_by_key(|e| e.file_name());
        v.into_iter().next()
    });
    if let (Some(conn), Some(dev)) = (conn, device) {
        let name = dev.file_name().to_string_lossy().into_owned();
        let max: u32 = std::fs::read_to_string(dev.path().join("max_brightness")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if max > 0 {
            let value = (max as u64 * percent as u64 / 100).max(1) as u32;
            if let Ok(p) = Proxy::new(conn, "org.freedesktop.login1", "/org/freedesktop/login1/session/auto", "org.freedesktop.login1.Session") {
                let r: Result<(), _> = p.call("SetBrightness", &("backlight", name.as_str(), value));
                if r.is_ok() {
                    return;
                }
            }
        }
    }
    let _ = Command::new("brightnessctl").args(["set", &format!("{percent}%")]).status();
}

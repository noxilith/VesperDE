//! Bluetooth through BlueZ's D-Bus API: the list of devices, searching, pairing, connecting and forgetting.
//! A worker thread owns the system-bus connection (and a pairing agent that accepts "just works" requests) and
//! publishes a snapshot; the bar reads it when it draws and is told by a channel when it changed.

use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},
};

use crate::dbus::Dbus;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub rssi: Option<i16>,
    /// What is happening to it right now ("pairing…", "connecting…", "failed").
    pub busy: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct State {
    /// There is a Bluetooth adapter and bluetoothd answers.
    pub available: bool,
    pub powered: bool,
    pub scanning: bool,
    /// Connected first, then the other paired devices, then the ones nearby by signal strength.
    pub devices: Vec<Device>,
}

pub enum Cmd {
    Scan(bool),
    Power(bool),
    Connect(String),
    Disconnect(String),
    Pair(String),
    Forget(String),
    /// Look again now (an operation finished).
    Refresh,
}

static TX: OnceLock<mpsc::Sender<Cmd>> = OnceLock::new();
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The latest picture of the adapter and its devices.
pub fn snapshot() -> State {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

pub fn send(cmd: Cmd) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(cmd);
    }
}

/// Start the worker (once). `notify` is told whenever the picture changed.
pub fn start(notify: Sender<Dbus>) {
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let again = tx.clone();
        std::thread::spawn(move || worker(rx, again, notify));
        tx
    });
}

struct Agent;

/// BlueZ asks an agent to confirm pairing; this one says yes to everything (the devices are ones you picked).
#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}
    fn request_pin_code(&self, _device: ObjectPath<'_>) -> String {
        "0000".into()
    }
    fn display_pin_code(&self, _device: ObjectPath<'_>, _pin: String) {}
    fn request_passkey(&self, _device: ObjectPath<'_>) -> u32 {
        0
    }
    fn display_passkey(&self, _device: ObjectPath<'_>, _passkey: u32, _entered: u16) {}
    fn request_confirmation(&self, _device: ObjectPath<'_>, _passkey: u32) {}
    fn request_authorization(&self, _device: ObjectPath<'_>) {}
    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: String) {}
    fn cancel(&self) {}
}

type Props = HashMap<String, OwnedValue>;
type Managed = HashMap<OwnedObjectPath, HashMap<String, Props>>;

fn text(p: &Props, k: &str) -> Option<String> {
    p.get(k).and_then(|v| String::try_from(v.clone()).ok())
}

fn flag(p: &Props, k: &str) -> bool {
    p.get(k).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false)
}

fn proxy<'a>(conn: &Connection, path: &str, iface: &'a str) -> Option<Proxy<'a>> {
    Proxy::new(conn, "org.bluez", path.to_owned(), iface).ok()
}

/// Read everything BlueZ knows: the adapter and its devices.
fn read(conn: &Connection, busy: &Mutex<HashMap<String, String>>) -> (State, Option<String>) {
    let Some(om) = proxy(conn, "/", "org.freedesktop.DBus.ObjectManager") else {
        return (State::default(), None);
    };
    let objects = match om.call::<_, _, Managed>("GetManagedObjects", &()) {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("bluetooth: GetManagedObjects failed: {e}");
            return (State::default(), None);
        }
    };
    let mut st = State::default();
    let mut adapter = None;
    let busy = busy.lock().unwrap();
    for (path, ifaces) in &objects {
        if let Some(a) = ifaces.get("org.bluez.Adapter1") {
            if adapter.is_none() {
                adapter = Some(path.to_string());
                st.available = true;
                st.powered = flag(a, "Powered");
                st.scanning = flag(a, "Discovering");
            }
        }
        if let Some(d) = ifaces.get("org.bluez.Device1") {
            let name = text(d, "Alias").or_else(|| text(d, "Name")).unwrap_or_default();
            let paired = flag(d, "Paired");
            // strangers: skip unnamed ones (they show as an address) and ones BlueZ only remembers from earlier (no signal)
            let rssi = d.get("RSSI").and_then(|v| i16::try_from(v.clone()).ok());
            if name.is_empty() || (!paired && (name.matches('-').count() == 5 || rssi.is_none())) {
                continue;
            }
            st.devices.push(Device {
                path: path.to_string(),
                name,
                paired,
                connected: flag(d, "Connected"),
                rssi,
                busy: busy.get(path.as_str()).cloned(),
            });
        }
    }
    st.devices.sort_by(|a, b| {
        (!a.connected, !a.paired, std::cmp::Reverse(a.rssi.unwrap_or(i16::MIN)), &a.name).cmp(&(!b.connected, !b.paired, std::cmp::Reverse(b.rssi.unwrap_or(i16::MIN)), &b.name))
    });
    (st, adapter)
}

fn register_agent(conn: &Connection) {
    let path = "/org/vesper/agent";
    if conn.object_server().at(path, Agent).is_err() {
        return;
    }
    if let Some(m) = proxy(conn, "/org/bluez", "org.bluez.AgentManager1") {
        let p = ObjectPath::try_from(path).unwrap();
        let r: Result<(), zbus::Error> = m.call("RegisterAgent", &(&p, "NoInputNoOutput"));
        if let Err(e) = &r {
            tracing::info!("bluetooth: RegisterAgent: {e}");
        }
        let r: Result<(), zbus::Error> = m.call("RequestDefaultAgent", &(&p,));
        if let Err(e) = &r {
            tracing::info!("bluetooth: no default agent: {e}");
        }
    }
}

fn worker(rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<Cmd>, notify: Sender<Dbus>) {
    let conn = match Connection::system() {
        Ok(c) => c,
        Err(e) => {
            tracing::info!("bluetooth: system bus: {e}");
            return;
        }
    };
    register_agent(&conn);
    let busy: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let mut scan_started: Option<Instant> = None;
    let mut first = true;
    loop {
        // look straight away the first time, then every 4 s (every 0.8 s while searching) or when told something
        let wait = if std::mem::take(&mut first) { Duration::ZERO } else if scan_started.is_some() { Duration::from_millis(800) } else { Duration::from_secs(4) };
        let cmd = match rx.recv_timeout(wait) {
            Ok(c) => Some(c),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let (_, adapter) = read(&conn, &busy);
        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Scan(on) => {
                    if let Some(a) = adapter.as_deref().and_then(|a| proxy(&conn, a, "org.bluez.Adapter1")) {
                        let _: Result<(), _> = a.call(if on { "StartDiscovery" } else { "StopDiscovery" }, &());
                    }
                    scan_started = on.then(Instant::now);
                }
                Cmd::Power(on) => {
                    if let Some(a) = adapter.as_deref().and_then(|a| proxy(&conn, a, "org.bluez.Adapter1")) {
                        let _ = a.set_property("Powered", on);
                    }
                }
                Cmd::Forget(path) => {
                    if let (Some(a), Ok(p)) = (adapter.as_deref().and_then(|a| proxy(&conn, a, "org.bluez.Adapter1")), ObjectPath::try_from(path.as_str())) {
                        let _: Result<(), _> = a.call("RemoveDevice", &(&p,));
                    }
                }
                Cmd::Connect(path) => spawn_op(Op::Connect, path, &conn, &busy, &tx),
                Cmd::Disconnect(path) => spawn_op(Op::Disconnect, path, &conn, &busy, &tx),
                Cmd::Pair(path) => spawn_op(Op::Pair, path, &conn, &busy, &tx),
                Cmd::Refresh => {}
            }
        }
        // searching stops by itself after a minute
        if scan_started.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            if let Some(a) = adapter.as_deref().and_then(|a| proxy(&conn, a, "org.bluez.Adapter1")) {
                let _: Result<(), _> = a.call("StopDiscovery", &());
            }
            scan_started = None;
        }
        let (state, _) = read(&conn, &busy);
        let changed = STATE.lock().unwrap().as_ref() != Some(&state);
        if changed {
            *STATE.lock().unwrap() = Some(state);
            if notify.send(Dbus::Bt).is_err() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    Connect,
    Disconnect,
    Pair,
}

/// These can take many seconds: do them on their own thread and show what is going on meanwhile.
fn spawn_op(op: Op, path: String, conn: &Connection, busy: &Arc<Mutex<HashMap<String, String>>>, tx: &mpsc::Sender<Cmd>) {
    let (conn, busy, tx) = (conn.clone(), busy.clone(), tx.clone());
    std::thread::spawn(move || operate(&conn, &busy, &tx, op, path));
}

fn operate(conn: &Connection, busy: &Mutex<HashMap<String, String>>, tx: &mpsc::Sender<Cmd>, op: Op, path: String) {
    let set = |s: Option<&str>| {
        let mut b = busy.lock().unwrap();
        match s {
            Some(t) => {
                b.insert(path.clone(), t.to_string());
            }
            None => {
                b.remove(&path);
            }
        }
        let _ = tx.send(Cmd::Refresh);
    };
    let Some(dev) = proxy(conn, &path, "org.bluez.Device1") else { return };
    let result: Result<(), zbus::Error> = match op {
        Op::Disconnect => {
            set(Some("disconnecting\u{2026}"));
            dev.call("Disconnect", &())
        }
        Op::Connect => {
            set(Some("connecting\u{2026}"));
            dev.call("Connect", &())
        }
        Op::Pair => {
            set(Some("pairing\u{2026}"));
            let paired: Result<(), zbus::Error> = dev.call("Pair", &());
            // already paired is fine; then trust it (so it reconnects by itself later) and connect
            if paired.is_ok() || paired.as_ref().is_err_and(|e| e.to_string().contains("AlreadyExists")) {
                let _ = dev.set_property("Trusted", true);
                set(Some("connecting\u{2026}"));
                dev.call("Connect", &())
            } else {
                paired
            }
        }
    };
    match result {
        Ok(()) => set(None),
        Err(e) => {
            tracing::info!("bluetooth: {e}");
            set(Some("failed"));
            std::thread::sleep(Duration::from_secs(3));
            set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::channel;

    /// Needs bluetoothd and an adapter: `cargo test -p vesper-bar -- --ignored bluetooth --nocapture`.
    #[test]
    #[ignore]
    fn bluetooth_lists_and_scans() {
        let (tx, _rx) = channel::channel::<Dbus>();
        start(tx);
        std::thread::sleep(Duration::from_millis(1500));
        let s = snapshot();
        println!("available={} powered={} scanning={} devices={}", s.available, s.powered, s.scanning, s.devices.len());
        assert!(s.available, "no adapter found");
        if s.powered {
            send(Cmd::Scan(true));
            std::thread::sleep(Duration::from_secs(6));
            let s = snapshot();
            println!("scanning={} devices:", s.scanning);
            for d in &s.devices {
                println!("  {:<28} paired={} connected={} rssi={:?}", d.name, d.paired, d.connected, d.rssi);
            }
            assert!(s.scanning);
            send(Cmd::Scan(false));
            std::thread::sleep(Duration::from_millis(1500));
            assert!(!snapshot().scanning);
        }
    }
}

/// For the popup sample sheet.
#[cfg(test)]
pub fn set_state_for_test(s: State) {
    *STATE.lock().unwrap() = Some(s);
}

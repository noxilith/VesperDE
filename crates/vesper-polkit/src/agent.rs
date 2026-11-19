//! The D-Bus side: `org.freedesktop.PolicyKit1.AuthenticationAgent` on the system bus, registered with polkitd for
//! this logind session. Requests are handed to the UI thread; the method call returns when the UI is done.

use std::{collections::HashMap, process::Command, time::Duration};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedValue, Value},
    DBusError,
};

pub const AGENT_PATH: &str = "/org/vesper/PolicyKit1/AuthenticationAgent";
const POLKIT: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY: &str = "/org/freedesktop/PolicyKit1/Authority";

/// A user that may authenticate this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub uid: u32,
}

/// What polkit asked for.
pub struct Request {
    pub action_id: String,
    pub message: String,
    pub cookie: String,
    pub identities: Vec<Identity>,
    /// The method call waits for this.
    pub reply: async_channel::Sender<Result<(), Failure>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
}

pub enum Msg {
    Begin(Request),
    Cancel(String),
}

#[derive(DBusError, Debug)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
pub enum PkError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

struct Agent {
    tx: Sender<Msg>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), PkError> {
        let identities = resolve_identities(&identities);
        if identities.is_empty() {
            return Err(PkError::Failed("no user to authenticate as".into()));
        }
        let (reply, wait) = async_channel::bounded(1);
        tracing::debug!("icon {icon_name:?}, details {details:?}");
        let req = Request { action_id, message, cookie, identities, reply };
        self.tx.send(Msg::Begin(req)).map_err(|_| PkError::Failed("the dialog is gone".into()))?;
        match wait.recv().await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(Failure::Cancelled)) | Err(_) => Err(PkError::Cancelled("dismissed".into())),
        }
    }

    async fn cancel_authentication(&self, cookie: String) -> Result<(), PkError> {
        let _ = self.tx.send(Msg::Cancel(cookie));
        Ok(())
    }
}

/// `a(sa{sv})` from polkit -> users. `unix-user` has `uid`, `unix-group` has `gid` (expanded to its members).
/// The current user comes first, then root-or-others in the order polkit gave them.
fn resolve_identities(raw: &[(String, HashMap<String, OwnedValue>)]) -> Vec<Identity> {
    let mut out: Vec<Identity> = Vec::new();
    for (kind, props) in raw {
        match kind.as_str() {
            "unix-user" => {
                if let Some(uid) = props.get("uid").and_then(|v| u32::try_from(v.clone()).ok()) {
                    if let Some(id) = user_by_uid(uid) {
                        out.push(id);
                    }
                }
            }
            "unix-group" => {
                if let Some(gid) = props.get("gid").and_then(|v| u32::try_from(v.clone()).ok()) {
                    for name in group_members(gid) {
                        if let Some(id) = user_by_name(&name) {
                            out.push(id);
                        }
                    }
                }
            }
            other => tracing::debug!("ignoring identity kind {other}"),
        }
    }
    order_identities(out, unsafe { libc::getuid() })
}

/// Drop duplicates and put `me` first.
pub fn order_identities(mut ids: Vec<Identity>, me: u32) -> Vec<Identity> {
    let mut seen = Vec::new();
    ids.retain(|i| {
        let new = !seen.contains(&i.uid);
        seen.push(i.uid);
        new
    });
    ids.sort_by_key(|i| i.uid != me); // stable: false (= me) sorts first
    ids
}

/// `name:x:uid:gid:gecos:home:shell`
pub fn parse_passwd(line: &str) -> Option<Identity> {
    let mut f = line.trim().split(':');
    let name = f.next().filter(|n| !n.is_empty())?;
    f.next()?;
    let uid = f.next()?.parse().ok()?;
    Some(Identity { name: name.to_string(), uid })
}

/// `name:x:gid:member,member`
pub fn parse_group(line: &str) -> Vec<String> {
    line.trim().split(':').nth(3).unwrap_or("").split(',').filter(|m| !m.is_empty()).map(str::to_string).collect()
}

fn getent(db: &str, key: &str) -> Option<String> {
    let out = Command::new("getent").arg(db).arg(key).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string())
}

fn user_by_uid(uid: u32) -> Option<Identity> {
    parse_passwd(&getent("passwd", &uid.to_string())?)
}

fn user_by_name(name: &str) -> Option<Identity> {
    parse_passwd(&getent("passwd", name)?)
}

fn group_members(gid: u32) -> Vec<String> {
    getent("group", &gid.to_string()).map(|l| parse_group(&l)).unwrap_or_default()
}

/// The logind session this process belongs to: `$XDG_SESSION_ID`, else ask logind.
fn session_id(sys: &Connection) -> Option<String> {
    if let Some(id) = std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty()) {
        return Some(id);
    }
    let mgr = Proxy::new(sys, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager").ok()?;
    let path: zbus::zvariant::OwnedObjectPath = mgr.call("GetSessionByPID", &(std::process::id(),)).ok()?;
    let session = Proxy::new(sys, "org.freedesktop.login1", path, "org.freedesktop.login1.Session").ok()?;
    session.get_property::<String>("Id").ok()
}

/// Who the agent registers for: the logind session, or (no logind session, e.g. PAM without `pam_systemd`) this process.
#[derive(Debug, PartialEq)]
enum Subject {
    Session(String),
    Process { pid: u32, start: u64 },
}

/// Start time (clock ticks since boot) of a process, field 22 of `/proc/<pid>/stat`. The command name in field 2 may
/// contain spaces and parentheses, so count from the last `)`.
fn proc_start_time(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

fn subject(sys: &Connection) -> Option<Subject> {
    if let Some(id) = session_id(sys) {
        return Some(Subject::Session(id));
    }
    let pid = std::process::id();
    let start = proc_start_time(&std::fs::read_to_string("/proc/self/stat").ok()?)?;
    Some(Subject::Process { pid, start })
}

fn register(sys: &Connection, who: &Subject) -> zbus::Result<()> {
    let authority = Proxy::new(sys, POLKIT, AUTHORITY, "org.freedesktop.PolicyKit1.Authority")?;
    let mut subject: HashMap<&str, Value> = HashMap::new();
    let kind = match who {
        Subject::Session(id) => {
            subject.insert("session-id", Value::from(id.as_str()));
            "unix-session"
        }
        Subject::Process { pid, start } => {
            subject.insert("pid", Value::from(*pid));
            subject.insert("start-time", Value::from(*start));
            "unix-process"
        }
    };
    let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into());
    authority.call::<_, _, ()>("RegisterAuthenticationAgent", &((kind, subject), locale, AGENT_PATH))
}

fn owner(sys: &Connection) -> Option<String> {
    let bus = Proxy::new(sys, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus").ok()?;
    bus.call::<_, _, String>("GetNameOwner", &(POLKIT,)).ok()
}

/// Serve the agent forever: connect, export it, register with polkitd, and register again if polkitd restarts.
pub fn run(tx: Sender<Msg>) {
    let sys = loop {
        match Connection::system() {
            Ok(c) => break c,
            Err(e) => {
                tracing::warn!("no system bus: {e}");
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    };
    if let Err(e) = sys.object_server().at(AGENT_PATH, Agent { tx }) {
        tracing::error!("could not export the agent: {e}");
        return;
    }
    let mut registered_with: Option<String> = None;
    loop {
        match (owner(&sys), subject(&sys)) {
            (Some(o), Some(who)) => {
                if registered_with.as_deref() != Some(o.as_str()) {
                    match register(&sys, &who) {
                        Ok(()) => {
                            tracing::info!("registered as the polkit agent ({who:?})");
                            registered_with = Some(o);
                        }
                        Err(e) => tracing::warn!("polkit registration failed: {e}"),
                    }
                }
            }
            (None, _) => {
                if registered_with.take().is_some() {
                    tracing::warn!("polkitd went away; will register again when it is back");
                }
            }
            (_, None) => tracing::warn!("could not work out who to register the polkit agent for"),
        }
        std::thread::sleep(Duration::from_secs(if registered_with.is_some() { 10 } else { 5 }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_survives_odd_command_names() {
        let stat = "123 (my) prog) S 1 123 123 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 100 18446744073709551615";
        assert_eq!(proc_start_time(stat), Some(987654));
    }

    #[test]
    fn parses_passwd_and_group_lines() {
        assert_eq!(parse_passwd("root:x:0:0:root:/root:/bin/bash"), Some(Identity { name: "root".into(), uid: 0 }));
        assert_eq!(parse_passwd("noxilith:x:1001:1001:N:/home/noxilith:/usr/bin/fish\n"), Some(Identity { name: "noxilith".into(), uid: 1001 }));
        assert_eq!(parse_passwd("garbage"), None);
        assert_eq!(parse_group("wheel:x:998:alice,bob"), vec!["alice", "bob"]);
        assert!(parse_group("wheel:x:998:").is_empty());
    }

    #[test]
    fn current_user_is_listed_first_without_duplicates() {
        let id = |n: &str, u| Identity { name: n.into(), uid: u };
        let out = order_identities(vec![id("root", 0), id("bob", 1002), id("alice", 1001), id("bob", 1002)], 1001);
        assert_eq!(out, vec![id("alice", 1001), id("root", 0), id("bob", 1002)]);
        // not in the list (a plain "root only" action): polkit's order is kept
        assert_eq!(order_identities(vec![id("root", 0)], 1001), vec![id("root", 0)]);
    }
}

//! The org.freedesktop.Notifications service. Runs on zbus's own thread and forwards to the UI thread.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, Ordering},
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// 0 low, 1 normal, 2 critical
    pub urgency: u8,
    /// As requested: -1 = server default, 0 = never expires, otherwise milliseconds.
    pub timeout_ms: i32,
    /// Buttons the sender offered: (key, label).
    pub actions: Vec<(String, String)>,
    /// True if this replaces an existing notification with the same id.
    pub replaces: bool,
}

pub enum Msg {
    Notify(Notification),
    Close(u32),
    ToggleCenter,
    ClearHistory,
    SetDnd(bool),
}

pub struct Service {
    pub tx: Sender<Msg>,
    pub next_id: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Service {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body", "icon-static", "actions"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 { replaces_id } else { self.next_id.fetch_add(1, Ordering::Relaxed) };
        let urgency = hints.get("urgency").and_then(|v| u8::try_from(v).ok()).unwrap_or(1);
        // Some apps send only a desktop-entry hint and no icon.
        let icon = if !app_icon.is_empty() {
            app_icon
        } else {
            hints.get("desktop-entry").and_then(|v| <&str>::try_from(v).ok()).map(String::from).unwrap_or_default()
        };
        let _ = self.tx.send(Msg::Notify(Notification {
            id,
            app_name,
            icon,
            summary,
            body,
            urgency,
            timeout_ms: expire_timeout,
            actions: actions.chunks(2).filter(|c| c.len() == 2).map(|c| (c[0].clone(), c[1].clone())).collect(),
            replaces: replaces_id != 0,
        }));
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(Msg::Close(id));
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("vesper-notify".into(), "Vesper".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

/// Why a notification went away, per the spec.
#[derive(Clone, Copy)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

pub fn emit_closed(conn: &zbus::blocking::Connection, id: u32, reason: Reason) {
    let result = conn.emit_signal(
        None::<&str>,
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "NotificationClosed",
        &(id, reason as u32),
    );
    if let Err(err) = result {
        tracing::debug!("could not emit NotificationClosed: {err}");
    }
}

/// Markup in bodies is limited to a few tags; we show plain text.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup() {
        assert_eq!(strip_markup("<b>Hi</b> &amp; <i>bye</i>"), "Hi & bye");
    }
}

/// Tell the sender a button was pressed.
pub fn emit_action(conn: &zbus::blocking::Connection, id: u32, key: &str) {
    let result = conn.emit_signal(
        None::<&str>,
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "ActionInvoked",
        &(id, key),
    );
    if let Err(err) = result {
        tracing::debug!("could not emit ActionInvoked: {err}");
    }
}

/// Vesper's own controls: open the notification centre, clear it, do-not-disturb.
pub struct Control {
    pub tx: Sender<Msg>,
}

#[zbus::interface(name = "org.vesper.Notify")]
impl Control {
    fn toggle_center(&self) {
        let _ = self.tx.send(Msg::ToggleCenter);
    }
    fn clear_history(&self) {
        let _ = self.tx.send(Msg::ClearHistory);
    }
    fn set_do_not_disturb(&self, on: bool) {
        let _ = self.tx.send(Msg::SetDnd(on));
    }
}

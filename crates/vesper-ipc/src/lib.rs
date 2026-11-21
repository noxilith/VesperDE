//! Protocol between the compositor and shell components (bar, dock, ...).
//!
//! One Unix socket (`$VESPER_SOCKET`). Every line is one JSON value.
//! Compositor -> client: a [`State`], sent on connect and whenever it changes.
//! Client -> compositor: a [`Command`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct State {
    pub workspaces: Vec<Workspace>,
    /// Index into `workspaces`.
    pub active: usize,
    pub focused: Option<WindowInfo>,
    /// Minimized windows, oldest first. `Command::Restore { index }` indexes this list.
    #[serde(default)]
    pub minimized: Vec<WindowInfo>,
    /// The workspace each minimized window came from (same order as `minimized`).
    #[serde(default)]
    pub minimized_ws: Vec<usize>,
    /// Connected monitors.
    #[serde(default)]
    pub outputs: Vec<OutputInfo>,
    /// Short name of the active keyboard layout (for example "us") and how many layouts are configured.
    #[serde(default)]
    pub kb_layout: String,
    #[serde(default)]
    pub kb_layouts: usize,
    /// Counts presses of the start-menu key (Mod+Escape): the bar opens the start menu when it changes.
    #[serde(default)]
    pub start_menu: u32,
    /// True while the push-to-talk key is held (the bar shows the live microphone at once, before it can ask the sound server).
    #[serde(default)]
    pub ptt_live: bool,
    /// Counts presses of the volume and brightness keys, and which kind (`"volume"` or `"brightness"`) came last: the bar shows an on-screen popup.
    #[serde(default)]
    pub osd_seq: u32,
    #[serde(default)]
    pub osd_kind: String,
    /// Workspaces of every monitor (each monitor has its own set). The fields above are those of the focused monitor.
    #[serde(default)]
    pub monitors: Vec<MonitorState>,
}

/// The workspaces of one monitor.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MonitorState {
    /// Output name (`DP-1`).
    pub output: String,
    pub workspaces: Vec<Workspace>,
    pub active: usize,
    pub focused: Option<WindowInfo>,
    #[serde(default)]
    pub minimized: Vec<WindowInfo>,
    #[serde(default)]
    pub minimized_ws: Vec<usize>,
    /// This is the monitor the keyboard focus (and the pointer) is on.
    #[serde(default)]
    pub focused_output: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    /// Current mode as `WxH@Hz`.
    pub mode: String,
    /// Every mode the monitor offers, as `WxH@Hz`.
    pub modes: Vec<String>,
    pub scale: f64,
    pub x: i32,
    pub y: i32,
    /// Logical size.
    pub w: i32,
    pub h: i32,
    pub enabled: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Workspace {
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WindowInfo {
    pub app_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Switch to workspace `index` (0-based).
    Workspace { index: usize, #[serde(default)] output: Option<String> },
    /// Focus the column `index` of workspace `workspace` (switching to that workspace).
    Focus { workspace: usize, index: usize },
    /// Minimize the focused window.
    Minimize,
    /// Take a screenshot. `kind` is "screen" or "window", or "region" with `x`, `y`, `w`, `h` (output coordinates).
    Screenshot { kind: String, x: Option<i32>, y: Option<i32>, w: Option<i32>, h: Option<i32> },
    /// Float the focused window, or tile it again.
    ToggleFloating,
    /// Restore minimized window `index` (see `State::minimized`).
    Restore { index: usize, #[serde(default)] output: Option<String> },
    /// Lock the screen.
    Lock,
    /// Lock (if configured) and suspend the machine.
    Suspend,
    /// End the session.
    Logout,
    /// Open or close the notification centre.
    NotificationCenter,
    /// Open or close the power menu.
    PowerMenu,
    /// Focus the window at `index` of workspace `workspace` (the order of `State::workspaces`), switching to it.
    FocusWindow { workspace: usize, index: usize },
    /// Learn the push-to-talk key: the next key pressed becomes it.
    LearnPttKey,
    /// Open (or close) the wallpaper changer.
    Wallpapers,
    /// Put `text` on the clipboard (when not empty) and show a notification: used by the text and QR pickers.
    Clipboard { text: String, summary: String, body: String },
    /// Open (or close) the app launcher.
    Launcher,
    /// Make the bar show its volume or brightness popup (`"volume"` or `"brightness"`).
    ShowOsd { kind: String },
    /// Open the launcher in a mode ("clipboard" or "emoji"), or close it if it is open.
    LauncherMode { mode: String },
    /// Open the file manager.
    Files,
    /// Switch to the next keyboard layout.
    CycleLayout,
    /// The bar tells the compositor which parts of its surface are drawn (for blur): rectangles `[x, y, w, h]` in surface pixels.
    BarShapes { rects: Vec<[i32; 4]> },
    /// Move the focused window into the column on its left (stacking it), or out of its column.
    ConsumeLeft,
    Expel,
    /// Focus the window above / below in the focused column (-1 / 1).
    FocusInColumn { delta: isize },
    /// Open the settings window, optionally on a page.
    Settings { page: Option<String> },
    /// Do what a keyboard shortcut does, by its id (the ids of `vesper_config::keys::BINDS`: `close`, `fullscreen`, `terminal` ...).
    Action { id: String },
    /// Start a program (through `sh -c`).
    Spawn { command: String },
    /// Change a setting in the config file, like Settings does (for example `{"cmd":"set_config","path":"appearance.gap","value":8}`).
    SetConfig { path: String, value: serde_json::Value },
}

/// Where the compositor listens: `$VESPER_SOCKET`, else `$XDG_RUNTIME_DIR/vesper.sock`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("VESPER_SOCKET") {
        return p.into();
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    dir.join("vesper.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = State { workspaces: vec![Workspace::default(); 2], active: 1, focused: None, minimized: vec![], minimized_ws: vec![], outputs: vec![], kb_layout: String::new(), kb_layouts: 0, start_menu: 0, ptt_live: false, osd_seq: 0, osd_kind: String::new(), monitors: vec![] };
        let line = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<State>(&line).unwrap(), s);
        let c: Command = serde_json::from_str(r#"{"cmd":"workspace","index":2}"#).unwrap();
        assert_eq!(c, Command::Workspace { index: 2, output: None });
        let f: Command = serde_json::from_str(r#"{"cmd":"focus","workspace":1,"index":0}"#).unwrap();
        assert_eq!(f, Command::Focus { workspace: 1, index: 0 });
    }
}

/// While this file exists in the runtime directory the computer does not dim, lock or sleep because of idleness
/// ("keep awake" in the bar's quick settings). It disappears with the session.
pub fn keep_awake_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    dir.join("vesper-keep-awake")
}

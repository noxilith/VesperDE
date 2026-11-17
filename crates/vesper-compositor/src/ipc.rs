//! IPC server: pushes layout state to shell components and accepts their commands.

use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use smithay::{
    desktop::Window,
    reexports::calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction},
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};
use vesper_ipc::{Command, State, WindowInfo, Workspace};

use crate::state::Vesper;

pub struct Ipc {
    pub path: PathBuf,
    clients: Vec<(u64, UnixStream)>,
    next_id: u64,
    last: String,
    /// Title and app id last seen per toplevel, so a commit only triggers a broadcast when they changed.
    seen: std::collections::HashMap<smithay::reexports::wayland_server::backend::ObjectId, WindowInfo>,
}

impl Drop for Ipc {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn init(event_loop: &EventLoop<'static, Vesper>, state: &mut Vesper, path: &Path) -> Result<()> {
    let _ = std::fs::remove_file(path); // stale socket from a crashed run
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    listener.set_nonblocking(true)?;
    state.ipc = Some(Ipc { path: path.to_owned(), clients: Vec::new(), next_id: 0, last: String::new(), seen: Default::default() });

    let handle = event_loop.handle();
    let loop_handle = handle.clone();
    handle
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), move |_, listener, state| {
            while let Ok((stream, _)) = listener.accept() {
                state.ipc_accept(stream, &loop_handle);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

impl Vesper {
    fn ipc_accept(&mut self, stream: UnixStream, loop_handle: &smithay::reexports::calloop::LoopHandle<'static, Vesper>) {
        let _ = stream.set_nonblocking(true);
        let Ok(writer) = stream.try_clone() else { return };
        let json = serde_json::to_string(&self.ipc_state()).unwrap_or_default();
        let Some(ipc) = self.ipc.as_mut() else { return };
        let id = ipc.next_id;
        ipc.next_id += 1;
        let mut writer = writer;
        if writeln!(writer, "{json}").is_err() {
            return;
        }
        ipc.clients.push((id, writer));

        let mut buf: Vec<u8> = Vec::new();
        let reader = Arc::new(stream);
        let _ = loop_handle.insert_source(Generic::new(reader, Interest::READ, Mode::Level), move |_, stream, state| {
            let mut chunk = [0u8; 1024];
            let mut r: &UnixStream = stream;
            loop {
                match r.read(&mut chunk) {
                    Ok(0) => {
                        state.ipc_drop(id);
                        return Ok(PostAction::Remove);
                    }
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        state.ipc_drop(id);
                        return Ok(PostAction::Remove);
                    }
                }
            }
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                match serde_json::from_slice::<Command>(&line) {
                    Ok(cmd) => state.ipc_command(cmd),
                    Err(err) => tracing::warn!("bad ipc command: {err}"),
                }
            }
            if buf.len() > 64 * 1024 {
                state.ipc_drop(id);
                return Ok(PostAction::Remove);
            }
            Ok(PostAction::Continue)
        });
    }

    fn ipc_drop(&mut self, id: u64) {
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.clients.retain(|(i, _)| *i != id);
        }
    }

    fn ipc_command(&mut self, cmd: Command) {
        if self.locked() {
            return;
        }
        match cmd {
            Command::Workspace { index } => {
                let n = self.layout.workspaces.len() - 1;
                self.with_workspace_slide(|s| s.layout.active = index.min(n));
            }
            Command::ToggleFloating => {
                let rect = self.default_float_rect();
                self.layout.toggle_floating(rect);
                self.layout_changed();
            }
            Command::Screenshot { kind, x, y, w, h } => {
                use crate::screenshot::CaptureKind;
                let kind = match (kind.as_str(), x, y, w, h) {
                    ("screen", ..) => Some(CaptureKind::Screen),
                    ("window", ..) => Some(CaptureKind::Window),
                    ("freeze", ..) => Some(CaptureKind::Freeze(crate::screenshot::Pick::Shot)),
                    ("freeze-text", ..) => Some(CaptureKind::Freeze(crate::screenshot::Pick::Text)),
                    ("freeze-qr", ..) => Some(CaptureKind::Freeze(crate::screenshot::Pick::Qr)),
                    ("frozen", Some(x), Some(y), Some(w), Some(h)) if w > 0 && h > 0 => {
                        self.finish_frozen(smithay::utils::Rectangle::new((x, y).into(), (w, h).into()));
                        None
                    }
                    ("region", Some(x), Some(y), Some(w), Some(h)) if w > 0 && h > 0 => {
                        Some(CaptureKind::Region(smithay::utils::Rectangle::new((x, y).into(), (w, h).into())))
                    }
                    _ => None,
                };
                if let Some(kind) = kind {
                    self.request_capture(kind);
                }
            }
            Command::Lock => self.start_lock(),
            Command::Suspend => self.suspend_now(),
            Command::Logout => self.loop_signal.stop(),
            Command::NotificationCenter => Self::run_component("vesper-notify", &["center"]),
            Command::PowerMenu => self.toggle_power_menu(),
            Command::FocusWindow { workspace, index } => {
                self.with_workspace_slide(|s| s.layout.focus_at(workspace, index));
                let f = self.layout.focused_window().cloned();
                self.focus_window(f);
                self.layout_changed();
            }
            Command::LearnPttKey => {
                self.ptt_learning = true;
                Self::run_component("vesper-notify", &["send", "Push to talk", "Press the key you want to hold to talk (Esc cancels)"]);
            }
            Command::Wallpapers => self.toggle_wallpaper_picker(),
            Command::Clipboard { text, summary, body } => self.copy_text(text, &summary, &body),
            Command::Launcher => self.toggle_launcher(),
            Command::ShowOsd { kind } => {
                self.osd_seq = self.osd_seq.wrapping_add(1);
                self.osd_kind = kind;
                self.ipc_broadcast();
            }
            Command::LauncherMode { mode } => self.toggle_launcher_mode(&mode),
            Command::Files => self.open_files(),
            Command::CycleLayout => {
                if let Some(kb) = self.seat.get_keyboard() {
                    kb.with_xkb_state(self, |mut ctx| {
                        let (n, cur) = {
                            let x = ctx.xkb().lock().unwrap();
                            (x.layouts().count() as u32, x.active_layout().0)
                        };
                        if n > 1 {
                            let next = (cur + 1) % n;
                            ctx.set_layout(smithay::input::keyboard::Layout(next));
                        }
                    });
                    self.ipc_broadcast();
                }
            }
            Command::BarShapes { rects } => self.bar_shapes = rects,
            Command::ConsumeLeft => {
                self.layout.consume_left();
                self.layout_changed();
            }
            Command::Expel => {
                self.layout.expel();
                self.layout_changed();
            }
            Command::FocusInColumn { delta } => {
                self.layout.focus_in_column(delta);
                self.layout_changed();
            }
            Command::Settings { page } => self.open_settings(page.as_deref()),
            Command::Minimize => {
                if self.layout.minimize_focused() {
                    self.layout_changed();
                }
            }
            Command::Restore { index } => {
                if self.layout.restore(index).is_some() {
                    self.layout_changed();
                }
            }
            Command::Focus { workspace, index } => {
                let last = self.layout.workspaces.len() - 1;
                self.layout.active = workspace.min(last);
                let ws = self.layout.ws_mut();
                if !ws.columns.is_empty() {
                    ws.focused = index.min(ws.columns.len() - 1);
                }
                self.layout_changed();
            }
        }
    }

    fn window_info(w: &Window) -> WindowInfo {
        if let Some(x) = w.x11_surface() {
            return WindowInfo { app_id: crate::xwayland::x11_app_id(x), title: x.title() };
        }
        let Some(top) = w.toplevel() else { return WindowInfo::default() };
        with_states(top.wl_surface(), |states| {
            let data = states.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
            WindowInfo { app_id: data.app_id.clone().unwrap_or_default(), title: data.title.clone().unwrap_or_default() }
        })
    }

    fn output_infos(&self) -> Vec<vesper_ipc::OutputInfo> {
        let primary = self.output().map(|o| o.name());
        let fmt = |m: smithay::output::Mode| format!("{}x{}@{:.0}", m.size.w, m.size.h, m.refresh as f64 / 1000.0);
        let mut v: Vec<_> = self
            .space
            .outputs()
            .map(|o| {
                let g = self.space.output_geometry(o).unwrap_or_default();
                let mut modes: Vec<String> = o.modes().into_iter().map(fmt).collect();
                modes.dedup();
                vesper_ipc::OutputInfo {
                    name: o.name(),
                    mode: o.current_mode().map(fmt).unwrap_or_default(),
                    modes,
                    scale: o.current_scale().fractional_scale(),
                    x: g.loc.x,
                    y: g.loc.y,
                    w: g.size.w,
                    h: g.size.h,
                    enabled: true,
                    primary: primary.as_deref() == Some(o.name().as_str()),
                }
            })
            .collect();
        v.extend(self.disabled_output_infos());
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn ipc_state(&self) -> State {
        State {
            workspaces: self
                .layout
                .workspaces
                .iter()
                .map(|ws| Workspace {
                    windows: ws.columns.iter().chain(ws.floating.iter().map(|(w, _)| w)).map(Self::window_info).collect(),
                })
                .collect(),
            active: self.layout.active,
            focused: self.layout.focused_window().map(Self::window_info),
            minimized: self.layout.minimized.iter().map(|(_, w)| Self::window_info(w)).collect(),
            minimized_ws: self.layout.minimized.iter().map(|(ws, _)| *ws).collect(),
            outputs: self.output_infos(),
            kb_layout: self.kb_layout_name(),
            start_menu: self.start_menu_seq,
            ptt_live: self.ptt_held,
            osd_seq: self.osd_seq,
            osd_kind: self.osd_kind.clone(),
            kb_layouts: self.config.input.xkb_layout.split(',').filter(|s| !s.trim().is_empty()).count(),
        }
    }

    /// Short name of the active keyboard layout, from the configured list.
    fn kb_layout_name(&self) -> String {
        let idx = self.seat.get_keyboard().map_or(0, |kb| kb.modifier_state().serialized.layout_effective as usize);
        self.config.input.xkb_layout.split(',').map(str::trim).filter(|s| !s.is_empty()).nth(idx).unwrap_or("").to_string()
    }

    /// Push the current state to every client if it changed since the last push.
    /// A surface committed: tell clients only if it is a window whose title or app id changed.
    pub fn ipc_surface_committed(&mut self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        use smithay::reexports::wayland_server::Resource;
        if self.ipc.is_none() {
            return;
        }
        let Some(window) = self.layout.window_for_surface(surface).cloned() else { return };
        let info = Self::window_info(&window);
        let ipc = self.ipc.as_mut().unwrap();
        if ipc.seen.get(&surface.id()) != Some(&info) {
            ipc.seen.insert(surface.id(), info);
            self.ipc_broadcast();
        }
    }

    pub fn ipc_broadcast(&mut self) {
        if self.ipc.is_none() {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.ipc_state()) else { return };
        let ipc = self.ipc.as_mut().unwrap();
        if ipc.last == json {
            return;
        }
        ipc.clients.retain_mut(|(_, s)| writeln!(s, "{json}").is_ok());
        ipc.last = json;
    }
}

impl Vesper {
    /// Apply the first matching `[[rules]]` entry to a window once its app id is known.
    pub fn apply_window_rules(&mut self, surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        use smithay::reexports::wayland_server::Resource;
        if self.config.rules.is_empty() {
            return;
        }
        let id = surface.id();
        if self.rules_done.contains(&id) {
            return;
        }
        let Some(window) = self.layout.window_for_surface(surface).cloned() else { return };
        let info = Self::window_info(&window);
        if info.app_id.is_empty() {
            return; // the app id arrives with a later commit
        }
        self.rules_done.push(id);
        let Some(rule) = self.config.rules.iter().find(|r| r.matches(&info.app_id, &info.title)).cloned() else { return };
        tracing::info!("window rule for {:?} ({:?}): {rule:?}", info.app_id, info.title);
        if rule.workspace >= 1 {
            self.layout.send_to_workspace(&window, rule.workspace - 1);
        }
        if rule.float {
            let mut rect = self.default_float_rect();
            if rule.width > 0 && rule.height > 0 {
                let (w, h) = (rule.width as i32, rule.height as i32);
                let c = (rect.loc.x + rect.size.w / 2, rect.loc.y + rect.size.h / 2);
                rect = smithay::utils::Rectangle::new((c.0 - w / 2, c.1 - h / 2).into(), (w, h).into());
            }
            self.layout.float_window(&window, rect);
        }
        self.layout_changed();
    }
}

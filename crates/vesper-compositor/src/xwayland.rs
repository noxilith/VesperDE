//! XWayland: runs an X server inside the compositor and acts as its window manager, so X11 apps (Steam, older Electron
//! and Java apps, games) get windows like any other: tiled, floating, fullscreen or minimized.
//!
//! Managed X11 windows live in the layout like xdg toplevels do. Override-redirect windows (menus, tooltips, drop-downs)
//! position themselves, so they are kept outside the layout, drawn on top of everything, and get no border.

use std::os::fd::OwnedFd;

use smithay::{
    desktop::Window,
    reexports::wayland_server::Client,
    utils::{Logical, Rectangle, Size},
    wayland::{
        selection::{
            data_device::{clear_data_device_selection, request_data_device_client_selection, set_data_device_selection, SelectionRequestError},
            SelectionTarget,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge, WmWindowProperty, WmWindowType, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::state::Vesper;

/// Is this one of those self-positioned X11 windows (menu, tooltip, drop-down)?
pub fn is_override_redirect(w: &Window) -> bool {
    w.x11_surface().is_some_and(|x| x.is_override_redirect())
}

/// The application id of an X11 window: its class (what .desktop files call `StartupWMClass`), else its instance name.
pub fn x11_app_id(x: &X11Surface) -> String {
    let class = x.class();
    if class.is_empty() {
        x.instance()
    } else {
        class
    }
}

/// Should a freshly mapped X11 window float, and at what size? Dialogs, tool windows, splash screens, transient windows
/// and fixed-size windows float; everything else takes a column.
fn float_size(x: &X11Surface) -> Option<Size<i32, Logical>> {
    let fixed = match (x.min_size(), x.max_size()) {
        (Some(min), Some(max)) => min.w > 0 && min.h > 0 && min == max,
        _ => false,
    };
    let kind = matches!(
        x.window_type(),
        Some(WmWindowType::Dialog | WmWindowType::Utility | WmWindowType::Splash | WmWindowType::Toolbar | WmWindowType::Notification)
    );
    if !(fixed || kind || x.is_transient_for().is_some()) {
        return None;
    }
    let g = x.geometry().size;
    Some(if g.w > 0 && g.h > 0 { g } else { (800, 600).into() })
}

impl Vesper {
    /// Start Xwayland (if `shell.xwayland` is on and the binary exists) and the window manager once it is ready.
    pub fn start_xwayland(&mut self) {
        if !self.config.shell.xwayland {
            tracing::info!("xwayland disabled in the config");
            return;
        }
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            std::process::Stdio::null(),
            std::process::Stdio::null(),
            |_| {},
        );
        let (xwayland, client) = match spawned {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("could not start Xwayland ({err}); X11 apps will not run (install the Xwayland package, or set shell.xwayland = false)");
                return;
            }
        };
        let handle = self.loop_handle.clone();
        let wm_client: Client = client;
        let res = self.loop_handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                match X11Wm::start_wm(handle.clone(), x11_socket, wm_client.clone()) {
                    Ok(wm) => {
                        state.xwm = Some(wm);
                        let disp = format!(":{display_number}");
                        tracing::info!("xwayland ready on DISPLAY={}", disp);
                        // everything we spawn from now on (and the session bus services) can reach X11 apps
                        std::env::set_var("DISPLAY", &disp);
                        let _ = std::process::Command::new("dbus-update-activation-environment")
                            .arg("DISPLAY")
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                        state.x11_display = Some(display_number);
                    }
                    Err(err) => tracing::warn!("could not attach the X11 window manager: {err}"),
                }
            }
            XWaylandEvent::Error => {
                tracing::warn!("Xwayland failed to start");
                state.xwm = None;
                state.x11_display = None;
            }
        });
        if let Err(err) = res {
            tracing::warn!("could not watch Xwayland: {err}");
        }
    }

    /// Tell Xwayland our clipboard changed (`None` = it is now empty), so X11 apps can paste what Wayland apps copied.
    pub fn announce_clipboard_to_x11(&mut self, mime_types: Option<Vec<String>>) {
        self.x11_clipboard = false;
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(SelectionTarget::Clipboard, mime_types) {
                tracing::debug!("xwayland clipboard: {err}");
            }
        }
    }

    /// An X11 window was unmapped or destroyed: take it out of the layout and the scene.
    fn x11_forget(&mut self, x: &X11Surface) {
        let _ = x.set_mapped(false);
        if x.is_override_redirect() {
            let gone: Vec<Window> = self.or_windows.iter().filter(|w| w.x11_surface() == Some(x)).cloned().collect();
            self.or_windows.retain(|w| !gone.contains(w));
            for w in gone {
                self.space.unmap_elem(&w);
            }
            return;
        }
        let Some(w) = self.layout.window_for_x11(x.window_id()).cloned() else { return };
        self.fx.forget(&w);
        self.space.unmap_elem(&w);
        if self.layout.remove_window(&w) {
            self.layout_changed();
        }
    }

    /// Make `window` fullscreen on its workspace.
    pub fn fullscreen_window(&mut self, window: &Window) {
        if self.layout.contains(window) {
            self.layout.focus_window(window);
            self.layout.ws_mut().fullscreen = Some(window.clone());
            self.layout_changed();
        }
    }

    pub fn unfullscreen_window(&mut self, window: &Window) {
        let mut changed = false;
        for ws in &mut self.layout.workspaces {
            if ws.fullscreen.as_ref() == Some(window) {
                ws.fullscreen = None;
                changed = true;
            }
        }
        if changed {
            self.layout_changed();
        }
    }

    pub fn minimize_window(&mut self, window: &Window) {
        if self.layout.find_window(window).is_some() || self.layout.workspaces.iter().any(|ws| ws.floating.iter().any(|(w, _)| w == window)) {
            self.layout.focus_window(window);
            if self.layout.minimize_focused() {
                self.layout_changed();
            }
        }
    }

    /// Ask a window to close (politely).
    pub fn close_window(window: &Window) {
        if let Some(top) = window.toplevel() {
            top.send_close();
        } else if let Some(x) = window.x11_surface() {
            let _ = x.close();
        }
    }
}

impl XWaylandShellHandler for Vesper {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}
smithay::delegate_xwayland_shell!(Vesper);

impl XwmHandler for Vesper {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("xwm event without a window manager")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if self.layout.window_for_x11(window.window_id()).is_some() {
            return;
        }
        if let Err(err) = window.set_mapped(true) {
            tracing::warn!("could not map an X11 window: {err}");
            return;
        }
        tracing::info!("new X11 window {:?} (class {:?})", window.title(), window.class());
        let win = Window::new_x11_window(window.clone());
        self.layout.add_window(win.clone());
        if let (Some(size), Some(out)) = (float_size(&window), self.output_geometry()) {
            let size: Size<i32, Logical> = (size.w.min(out.size.w), size.h.min(out.size.h)).into();
            let loc = (out.loc.x + (out.size.w - size.w) / 2, out.loc.y + (out.size.h - size.h) / 2);
            self.layout.float_window(&win, Rectangle::new(loc.into(), size));
        }
        self.layout_changed();
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let win = Window::new_x11_window(window.clone());
        self.space.map_element(win.clone(), window.geometry().loc, true);
        self.or_windows.push(win);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_forget(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_forget(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // A window in the layout has its size and place decided by the layout: just repeat what it already has.
        if self.layout.window_for_x11(window.window_id()).is_some() {
            let _ = window.configure(None);
            return;
        }
        let mut g = window.geometry();
        if let Some(x) = x {
            g.loc.x = x;
        }
        if let Some(y) = y {
            g.loc.y = y;
        }
        if let Some(w) = w {
            g.size.w = w as i32;
        }
        if let Some(h) = h {
            g.size.h = h as i32;
        }
        let _ = window.configure(g);
    }

    fn configure_notify(&mut self, _xwm: XwmId, window: X11Surface, geometry: Rectangle<i32, Logical>, _above: Option<u32>) {
        // Menus and tooltips move and resize themselves.
        if !window.is_override_redirect() {
            return;
        }
        if let Some(w) = self.or_windows.iter().find(|w| w.x11_surface() == Some(&window)).cloned() {
            self.space.map_element(w, geometry.loc, false);
        }
    }

    fn property_notify(&mut self, _xwm: XwmId, _window: X11Surface, property: WmWindowProperty) {
        // a new title or class changes what the bar shows
        if matches!(property, WmWindowProperty::Title | WmWindowProperty::Class) {
            self.ipc_broadcast();
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.layout.window_for_x11(window.window_id()).cloned() {
            self.fullscreen_window(&w);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.layout.window_for_x11(window.window_id()).cloned() {
            self.unfullscreen_window(&w);
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(w) = self.layout.window_for_x11(window.window_id()).cloned() {
            self.minimize_window(&w);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(i) = self.layout.minimized.iter().position(|(_, w)| w.x11_surface() == Some(&window)) {
            if self.layout.restore(i).is_some() {
                self.layout_changed();
            }
        }
    }

    // Tiled windows are not moved or resized by hand (floating ones use Mod+drag).
    fn resize_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32, _edge: ResizeEdge) {}
    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn allow_selection_access(&mut self, _xwm: XwmId, selection: SelectionTarget) -> bool {
        selection == SelectionTarget::Clipboard // there is no primary selection global to bridge to
    }

    /// An X11 app wants to read the Wayland clipboard.
    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        if selection != SelectionTarget::Clipboard {
            return;
        }
        let spare = fd.try_clone();
        match request_data_device_client_selection(&self.seat, mime_type.clone(), fd) {
            Ok(()) => {}
            // the compositor's own clipboard (screenshots, the text and QR pickers): serve it ourselves
            Err(SelectionRequestError::ServerSideSelection) => {
                if let Ok(spare) = spare {
                    self.send_clipboard_own(&mime_type, spare);
                }
            }
            Err(err) => tracing::debug!("x11 paste failed: {err:?}"),
        }
    }

    /// An X11 app copied: offer it to Wayland apps.
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        if selection == SelectionTarget::Clipboard {
            self.x11_clipboard = true;
            set_data_device_selection(&self.display_handle, &self.seat, mime_types, ());
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        if selection == SelectionTarget::Clipboard && self.x11_clipboard {
            self.x11_clipboard = false;
            clear_data_device_selection(&self.display_handle, &self.seat);
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        tracing::warn!("lost the connection to Xwayland");
        self.xwm = None;
        let gone: Vec<Window> = self
            .layout
            .workspaces
            .iter()
            .flat_map(|ws| ws.columns.iter().chain(ws.floating.iter().map(|(w, _)| w)))
            .chain(self.layout.minimized.iter().map(|(_, w)| w))
            .filter(|w| w.is_x11())
            .cloned()
            .collect();
        for w in gone {
            self.fx.forget(&w);
            self.space.unmap_elem(&w);
            self.layout.remove_window(&w);
        }
        for w in std::mem::take(&mut self.or_windows) {
            self.space.unmap_elem(&w);
        }
        self.layout_changed();
    }
}

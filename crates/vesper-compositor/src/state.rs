use std::{ffi::OsString, sync::Arc, time::Instant};

use smithay::{
    wayland::seat::WaylandFocus,
    backend::allocator::dmabuf::Dmabuf,
    desktop::{layer_map_for_output, PopupManager, Space, Window},
    input::{keyboard::XkbConfig, pointer::CursorImageStatus, Seat, SeatState},
    output::Output,
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Display, DisplayHandle,
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        dmabuf::DmabufState,
        idle_inhibit::IdleInhibitManagerState,
        session_lock::{LockSurface, SessionLockManagerState},
        shell::{wlr_layer::WlrLayerShellState, xdg::decoration::XdgDecorationState},
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use vesper_config::Config;

use crate::layout::{self, Layout};
use crate::effects::Effects;

/// An in-progress Mod+drag of a floating window.
pub struct Drag {
    pub window: Window,
    /// Resize (right button) instead of move (left button).
    pub resize: bool,
    pub start: Point<f64, Logical>,
    pub rect: Rectangle<i32, Logical>,
    /// The window is a tiled column: moving reorders columns, resizing changes the column width.
    pub tiled: bool,
    /// Resizing from the window's left / top half (the dragged edge is then the left / top one), like Hyprland.
    pub left: bool,
    pub top: bool,
    /// Horizontal offset of the strip when the drag began, kept fixed while resizing.
    pub base_shift: i32,
}

pub struct Vesper {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Vesper>,
    pub loop_signal: LoopSignal,

    pub config: Config,
    pub fx: Effects,
    pub blur: crate::blur::BlurState,
    pub round: Option<crate::round::RoundShaders>,
    /// Winit backend: ask the host window for a redraw (the tty backend draws from `redraw_all` directly).
    pub redraw_hook: Option<Box<dyn Fn()>>,
    /// Winit: the last pass of the event loop was only our own redraw, so it must not ask for another.
    pub just_redrew: bool,
    /// A throttled frame-callback timer is already armed (see `render` in the tty backend).
    pub frame_timer: bool,
    /// Per-output frame counters, written to the log every few seconds (see `render`).
    pub render_stats: std::collections::HashMap<String, RenderStats>,
    pub stats_logged: std::time::Instant,
    pub round_tried: bool,
    /// Monitors are powered down by the idle timer.
    pub screens_off: bool,
    /// Push to talk: the key (an xkb keysym) while it is on, whether it is held, and a counter for the first seconds.
    pub ptt_sym: Option<u32>,
    pub ptt_held: bool,
    pub ptt_down_at: Option<std::time::Instant>,
    /// A physical key code (xkb keycode) for keys named `code:N`, and "press the key now" mode.
    pub ptt_code: Option<u32>,
    pub ptt_learning: bool,
    /// Volume/brightness key presses (for the bar's on-screen popup) and which kind came last.
    pub osd_seq: u32,
    pub osd_kind: String,
    /// A three-finger swipe in progress: (dx, dy, fingers).
    pub swipe: Option<(f64, f64, u32)>,
    /// Windows whose rules were already applied.
    pub rules_done: Vec<smithay::reexports::wayland_server::backend::ObjectId>,
    /// The background programs of enabled plugins.
    pub plugins: crate::plugins::Running,
    pub ptt_boot: u32,
    /// The night-light setting last sent to the monitors, and whether they accepted it (otherwise a tint is drawn).
    pub night_applied: Option<Option<u32>>,
    pub night_via_gamma: bool,
    /// Running inside another compositor (winit backend) rather than on a real seat.
    pub nested: bool,
    /// Running as the login screen (`--greeter`): no shortcuts, only the greeter.
    pub greeter: bool,

    pub space: Space<Window>,
    pub layout: Layout,
    pub popups: PopupManager,
    pub cursor_status: CursorImageStatus,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    #[allow(dead_code)] // owns the wl_output / xdg_output globals
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Vesper>,
    pub data_device_state: DataDeviceState,
    /// Lets apps ask for a cursor by shape (arrow, text, hand ...) so it is drawn from our theme.
    #[allow(dead_code)]
    pub cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState,
    pub seat: Seat<Self>,

    /// Keys whose press was consumed by a binding; their release is swallowed too.
    pub suppressed_keys: Vec<u32>,
    /// Set by the tty backend: switch to another virtual terminal.
    pub vt_switch: Option<Box<dyn Fn(i32)>>,
    pub tty: Option<crate::backend::tty::Tty>,
    pub ipc: Option<crate::ipc::Ipc>,
    /// The running launcher, if any; the launcher key toggles it.
    pub launcher: Option<std::process::Child>,
    /// The running window switcher, if any (Mod+Tab).
    pub switcher: Option<std::process::Child>,
    /// Counts presses of the start-menu key; the bar opens the menu whenever it changes.
    pub start_menu_seq: u32,
    /// The running wallpaper changer, if any; Mod+W toggles it.
    pub wallpaper_picker: Option<std::process::Child>,
    pub power_menu: Option<std::process::Child>,
    pub settings: Option<std::process::Child>,
    pub drag: Option<Drag>,
    /// Resizing re-tiles every window, so it runs at most about 80 times a second; the last position is applied on release.
    pub last_drag_layout: Instant,
    /// Sub-pixel remainder of the Mod+Alt canvas pan.
    pub pan_frac: (f64, f64),
    pub drag_force: bool,
    /// The strip offset (alignment) used by the last layout.
    pub last_shift: i32,
    /// While a tiled window is being dragged: the box showing where it would land (logical, global coordinates).
    pub drop_hint: Option<Rectangle<i32, Logical>>,
    pub compact_pending: bool,
    /// The parts of the bar surface that are drawn, as the bar last reported them (surface pixels).
    pub bar_shapes: Vec<[i32; 4]>,
    /// How much the column being resized from its left edge has grown (so the release can keep the view where it is).
    pub resize_grown: i32,
    pub screencopy: crate::screencopy::ScreencopyState,
    /// The newer capture protocol (single windows too); None when switched off in the config.
    pub extcapture: Option<crate::extcapture::ExtCaptureState>,
    pub foreign_toplevel_list: smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    pub pending_capture: Option<crate::screenshot::CaptureKind>,
    pub shot_tx: smithay::reexports::calloop::channel::Sender<crate::screenshot::Done>,
    pub clipboard_png: Option<std::sync::Arc<Vec<u8>>>,
    /// Text from the text and QR pickers, served when an app pastes.
    pub clipboard_text: Option<String>,
    /// Xwayland's window manager, once it is up.
    pub xwm: Option<smithay::xwayland::X11Wm>,
    /// The Xwayland process watcher, kept so a restart can remove it, and how many restarts in a row have failed.
    pub xwayland_token: Option<smithay::reexports::calloop::RegistrationToken>,
    pub xwayland_restarts: u32,
    /// Which start of Xwayland is current, and where its stderr reader reports that it ended.
    pub xwayland_gen: u32,
    pub xwayland_exit_tx: Option<smithay::reexports::calloop::channel::Sender<u32>>,
    pub xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState,
    /// The X display number (`DISPLAY=:N`) once Xwayland is ready.
    pub x11_display: Option<u32>,
    /// Self-positioned X11 windows (menus, tooltips): drawn on top, outside the layout.
    pub or_windows: Vec<Window>,
    /// An X11 app owns the clipboard (a Wayland app pasting is served through Xwayland).
    pub x11_clipboard: bool,
    pub session_lock_state: SessionLockManagerState,
    /// Some = the session is locked; the lock client's surfaces.
    pub lock: Option<Vec<(LockSurface, Output)>>,
    pub lock_child: Option<std::process::Child>,
    pub last_lock_spawn: Instant,
    pub last_activity: Instant,
    pub idle_inhibitors: Vec<WlSurface>,
    #[allow(dead_code)] // owns the idle-inhibit global
    pub idle_inhibit_state: IdleInhibitManagerState,
    pub dmabuf_state: DmabufState,
    pub layer_shell_state: WlrLayerShellState,
    #[allow(dead_code)]
    pub fractional_scale_state: Option<smithay::wayland::fractional_scale::FractionalScaleManagerState>,
    #[allow(dead_code)]
    pub viewporter_state: Option<smithay::wayland::viewporter::ViewporterState>,
    #[allow(dead_code)] // owns the xdg-decoration global
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_foreign_state: smithay::wayland::xdg_foreign::XdgForeignState,
    pub primary_selection_state: smithay::wayland::selection::primary_selection::PrimarySelectionState,
    pub xdg_activation_state: smithay::wayland::xdg_activation::XdgActivationState,
    #[allow(dead_code)] // kept alive: dropping the state removes the global
    pub xdg_toplevel_icon_state: smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager,
    #[allow(dead_code)]
    pub text_input_state: smithay::wayland::text_input::TextInputManagerState,
    /// Winit backend: imports a dmabuf into its renderer to check it is usable.
    pub dmabuf_importer: Option<Box<dyn FnMut(&Dmabuf) -> bool>>,
}

impl Vesper {
    pub fn new(
        event_loop: &EventLoop<'static, Vesper>,
        display: Display<Vesper>,
        config: Config,
        nested: bool,
        seat_name: &str,
    ) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let xdg_foreign_state = smithay::wayland::xdg_foreign::XdgForeignState::new::<Self>(&dh);
        let primary_selection_state = smithay::wayland::selection::primary_selection::PrimarySelectionState::new::<Self>(&dh);
        let xdg_activation_state = smithay::wayland::xdg_activation::XdgActivationState::new::<Self>(&dh);
        let xdg_toplevel_icon_state = smithay::wayland::xdg_toplevel_icon::XdgToplevelIconManager::new::<Self>(&dh);
        let text_input_state = smithay::wayland::text_input::TextInputManagerState::new::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let cursor_shape_state = smithay::wayland::cursor_shape::CursorShapeManagerState::new::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name);

        let share_windows = config.screenshots.share_windows;
        let input = &config.input;
        let xkb = XkbConfig {
            layout: &input.xkb_layout,
            variant: &input.xkb_variant,
            options: (!input.xkb_options.is_empty()).then(|| input.xkb_options.clone()),
            ..Default::default()
        };
        if let Err(err) = seat.add_keyboard(xkb, input.repeat_delay, input.repeat_rate) {
            tracing::warn!("invalid xkb config ({err:?}), falling back to defaults");
            seat.add_keyboard(Default::default(), input.repeat_delay, input.repeat_rate).unwrap();
        }
        seat.add_pointer();

        let socket_name = Self::init_wayland_listener(display, event_loop);
        let display_handle_for_new = dh.clone();
        let (shot_tx, shot_rx) = smithay::reexports::calloop::channel::channel::<crate::screenshot::Done>();
        event_loop
            .handle()
            .insert_source(shot_rx, |event, _, state| {
                if let smithay::reexports::calloop::channel::Event::Msg(done) = event {
                    state.screenshot_done(done);
                }
            })
            .expect("failed to register the screenshot channel");

        Self {
            start_time: Instant::now(),
            socket_name,
            display_handle: dh,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            layout: Layout::new(config.layout.workspaces),
            fx: Effects::new(),
            blur: Default::default(),
            round: None,
            redraw_hook: None,
            just_redrew: false,
            frame_timer: false,
            render_stats: std::collections::HashMap::new(),
            stats_logged: std::time::Instant::now(),
            round_tried: false,
            screens_off: false,
            ptt_sym: None,
            ptt_held: false,
            ptt_down_at: None,
            ptt_code: None,
            ptt_learning: false,
            osd_seq: 0,
            osd_kind: String::new(),
            swipe: None,
            rules_done: Vec::new(),
            plugins: Default::default(),
            ptt_boot: 0,
            night_applied: None,
            night_via_gamma: false,
            config,
            nested,
            greeter: false,
            space: Space::default(),
            popups: PopupManager::default(),
            cursor_status: CursorImageStatus::default_named(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            cursor_shape_state,
            seat,
            suppressed_keys: Vec::new(),
            vt_switch: None,
            tty: None,
            ipc: None,
            launcher: None,
            switcher: None,
            start_menu_seq: 0,
            wallpaper_picker: None,
            power_menu: None,
            settings: None,
            drag: None,
            last_drag_layout: Instant::now(),
            pan_frac: (0.0, 0.0),
            drag_force: false,
            last_shift: 0,
            drop_hint: None,
            compact_pending: false,
            bar_shapes: Vec::new(),
            resize_grown: 0,
            screencopy: crate::screencopy::ScreencopyState::new(&display_handle_for_new),
            extcapture: share_windows.then(|| crate::extcapture::ExtCaptureState::new(&display_handle_for_new, true)),
            foreign_toplevel_list: smithay::wayland::foreign_toplevel_list::ForeignToplevelListState::new_with_filter::<Self>(&display_handle_for_new, move |_| share_windows),
            pending_capture: None,
            shot_tx,
            clipboard_png: None,
            clipboard_text: None,
            xwm: None,
            xwayland_token: None,
            xwayland_restarts: 0,
            xwayland_gen: 0,
            xwayland_exit_tx: None,
            xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState::new::<Self>(&display_handle_for_new),
            x11_display: None,
            or_windows: Vec::new(),
            x11_clipboard: false,
            session_lock_state: SessionLockManagerState::new::<Self, _>(&display_handle_for_new, |_| true),
            lock: None,
            lock_child: None,
            last_lock_spawn: Instant::now() - std::time::Duration::from_secs(60),
            last_activity: Instant::now(),
            idle_inhibitors: Vec::new(),
            idle_inhibit_state: IdleInhibitManagerState::new::<Self>(&display_handle_for_new),
            dmabuf_state: DmabufState::new(),
            layer_shell_state,
            fractional_scale_state: Some(smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(&display_handle_for_new)),
            viewporter_state: Some(smithay::wayland::viewporter::ViewporterState::new::<Self>(&display_handle_for_new)),
            xdg_decoration_state,
            xdg_foreign_state,
            primary_selection_state,
            xdg_activation_state,
            xdg_toplevel_icon_state,
            text_input_state,
            dmabuf_importer: None,
        }
    }

    fn init_wayland_listener(display: Display<Vesper>, event_loop: &EventLoop<'static, Vesper>) -> OsString {
        let listening_socket = ListeningSocketSource::new_auto().expect("failed to create wayland socket");
        let socket_name = listening_socket.socket_name().to_os_string();
        let handle = event_loop.handle();

        handle
            .insert_source(listening_socket, |stream, _, state| {
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                    tracing::warn!("failed to accept wayland client: {err}");
                }
            })
            .expect("failed to register wayland socket");

        handle
            .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
                // Safety: the display is never dropped while the loop runs.
                unsafe { display.get_mut().dispatch_clients(state).unwrap() };
                Ok(PostAction::Continue)
            })
            .expect("failed to register wayland display");

        socket_name
    }

    // ---------------------------------------------------------------- outputs

    /// The monitor that holds the tiled workspaces: `[display] primary`, else the first one.
    pub fn output(&self) -> Option<&Output> {
        let want = &self.config.display.primary;
        self.space.outputs().find(|o| !want.is_empty() && &o.name() == want).or_else(|| self.space.outputs().next())
    }

    /// Keep a pointer position inside the union of all monitors.
    pub fn clamp_to_outputs(&self, p: Point<f64, Logical>) -> Point<f64, Logical> {
        let rects: Vec<_> = self.space.outputs().filter_map(|o| self.space.output_geometry(o)).collect();
        if rects.iter().any(|r| r.to_f64().contains(p)) || rects.is_empty() {
            return p;
        }
        let mut best = p;
        let mut best_d = f64::MAX;
        for r in rects {
            let c = Point::from((p.x.clamp(r.loc.x as f64, (r.loc.x + r.size.w - 1) as f64), p.y.clamp(r.loc.y as f64, (r.loc.y + r.size.h - 1) as f64)));
            let d = (c.x - p.x).powi(2) + (c.y - p.y).powi(2);
            if d < best_d {
                best_d = d;
                best = c;
            }
        }
        best
    }

    pub fn output_geometry(&self) -> Option<Rectangle<i32, Logical>> {
        self.output().and_then(|o| self.space.output_geometry(o))
    }

    // ---------------------------------------------------------------- layout

    /// Recompute every window's size and position and map the active workspace.
    pub fn relayout(&mut self) {
        self.relayout_windows();
        // menus and tooltips stay above every window
        for w in self.or_windows.clone() {
            self.space.raise_element(&w, false);
        }
    }

    fn relayout_windows(&mut self) {
        let Some(out_geo) = self.output_geometry() else { return };
        // Tile inside whatever bars and docks (layer surfaces) leave free.
        let mut geo = self.output().map(|o| layer_map_for_output(o).non_exclusive_zone()).unwrap_or(out_geo);
        geo.loc += out_geo.loc;
        let gap = self.config.layout.gap;
        let default_ratio = self.config.layout.column_width;
        let height = layout::column_height(geo.size.h, gap);

        for (_, w) in &self.layout.minimized {
            self.space.unmap_elem(w);
        }
        let active = self.layout.active;
        let mut leaving: Vec<Window> = Vec::new();
        for (i, ws) in self.layout.workspaces.iter().enumerate() {
            if i != active {
                let all: Vec<Window> = ws.columns.iter().cloned().chain(ws.floating.iter().map(|(w, _)| w.clone())).collect();
                leaving.extend(all);
            }
        }
        for w in leaving {
            self.retire(&w);
        }

        let ws = &mut self.layout.workspaces[active];
        // Fullscreen covers the whole output, bars included; everything else steps aside.
        if let Some(fs) = ws.fullscreen.clone() {
            for w in ws.columns.iter().filter(|w| **w != fs) {
                self.space.unmap_elem(w);
            }
            for (w, _) in ws.floating.iter().filter(|(w, _)| *w != fs) {
                self.space.unmap_elem(w);
            }
            configure(&fs, out_geo.loc, out_geo.size.w, out_geo.size.h, WinMode::Fullscreen);
            self.place_now(fs, out_geo.loc);
            return;
        }

        let ranges = ws.column_ranges();
        let widths: Vec<i32> = ranges
            .iter()
            .map(|(s, _)| {
                let r = ws.ratios[*s];
                layout::column_width(geo.size.w, gap, if r == 0.0 { default_ratio } else { r })
            })
            .collect();
        let focused_col = ranges.iter().position(|(s, e)| (*s..=*e).contains(&ws.focused)).unwrap_or(0);
        let resizing = self.drag.as_ref().filter(|d| d.tiled && d.resize).map(|d| (d.window.clone(), d.rect.size.w, d.left, d.base_shift));
        if resizing.is_none() {
            ws.scroll = layout::solve_scroll(&widths, focused_col, ws.scroll, geo.size.w, gap);
        }
        let mut tiles = Vec::new();
        // a strip narrower than the screen can sit left, centred or right
        let strip = widths.iter().sum::<i32>() + gap * (widths.len() as i32 + 1);
        let spare = (geo.size.w - strip).max(0);
        let shift = match ws.align {
            layout::Align::Left => 0,
            layout::Align::Right => spare,
        };
        // While resizing, the view does not move by itself: the edge you grab follows the pointer and the opposite edge stays.
        let shift = match &resizing {
            Some((win, start_w, left, base)) => {
                let col = ws.columns.iter().position(|w| w == win).and_then(|i| ranges.iter().position(|(s, e)| (*s..=*e).contains(&i)));
                let grown = col.map_or(0, |c| widths[c] - *start_w);
                self.resize_grown = if *left { grown } else { 0 };
                *base - if *left { grown } else { 0 }
            }
            None => shift,
        };
        self.last_shift = if resizing.is_some() { self.last_shift } else { shift };
        for (ci, (s, e)) in ranges.iter().enumerate() {
            let x = geo.loc.x + layout::column_x(&widths, ci, gap) - ws.scroll + shift;
            // windows in a column share its height by weight, with a gap between them
            let n = (e - s + 1) as i32;
            let avail = (height - (n - 1) * gap).max(50 * n);
            let total: f64 = ws.weights[*s..=*e].iter().sum::<f64>().max(0.0001);
            let mut y = geo.loc.y + gap;
            for (k, i) in (*s..=*e).enumerate() {
                let h = if i == *e { geo.loc.y + gap + height - y } else { ((avail as f64) * ws.weights[i] / total).round() as i32 }.max(50);
                let _ = k;
                configure(&ws.columns[i], (x, y).into(), widths[ci], h, WinMode::Tiled);
                tiles.push((ws.columns[i].clone(), Point::from((x, y))));
                y += h + gap;
            }
        }
        let floats: Vec<_> = ws.floating.iter().map(|(w, r)| (w.clone(), r.loc)).collect();
        for (w, rect) in ws.floating.iter() {
            configure(w, rect.loc, rect.size.w, rect.size.h, WinMode::Floating);
        }
        let dragged = self.drag.as_ref().filter(|d| d.tiled && !d.resize).map(|d| d.window.clone());
        for (w, at) in tiles {
            if dragged.as_ref() == Some(&w) {
                continue; // it follows the pointer
            }
            self.place(w, at);
        }
        // Floating windows sit on top of the tiling, in their own stacking order.
        for (w, at) in floats {
            self.place_now(w.clone(), at);
            self.space.raise_element(&w, false);
        }
    }

    /// Give keyboard focus to `window` (or nobody) and update activated state.
    pub fn focus_window(&mut self, window: Option<Window>) {
        if self.locked() {
            return; // the lock screen keeps the keyboard
        }
        let serial = SERIAL_COUNTER.next_serial();
        let mut all: Vec<Window> = self.layout.workspaces.iter().flat_map(|ws| ws.columns.iter().cloned()).collect();
        all.extend(self.layout.workspaces.iter().flat_map(|ws| ws.floating.iter().map(|(w, _)| w.clone())));
        all.extend(self.layout.minimized.iter().map(|(_, w)| w.clone()));
        for w in &all {
            let on = window.as_ref() == Some(w);
            if w.set_activated(on) {
                if let Some(t) = w.toplevel() {
                    t.send_pending_configure();
                }
            }
        }
        let surface = window.as_ref().and_then(|w| w.wl_surface()).map(|s| s.into_owned());
        // an open launcher keeps the keyboard until it closes, even if a window maps or closes behind it
        if self.keyboard_on_layer() {
            return;
        }
        if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, surface, serial);
        }
    }

    /// With dynamic workspaces, tidy up a moment after the last change (so a quick hop between workspaces does not
    /// shuffle them under your feet).
    fn schedule_compact(&mut self) {
        if !self.config.layout.dynamic_workspaces || self.compact_pending {
            return;
        }
        self.compact_pending = true;
        let _ = self.loop_handle.insert_source(
            smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(1000)),
            |_, _, s| {
                s.compact_pending = false;
                if s.config.layout.dynamic_workspaces && s.layout.compact() {
                    s.layout_changed();
                }
                smithay::reexports::calloop::timer::TimeoutAction::Drop
            },
        );
    }

    /// After any layout change: re-tile and focus whatever the layout says is focused.
    pub fn layout_changed(&mut self) {
        self.schedule_compact();
        self.relayout();
        let focused = self.layout.focused_window().cloned();
        self.focus_window(focused);
        self.ipc_broadcast();
    }

    /// Start a Vesper component (`vesper-bar`, `vesper-launcher`, ...), preferring the copy next to this binary.
    /// Run a Vesper component with arguments and reap it.
    pub fn run_component(name: &str, args: &[&str]) {
        match Self::component_command(name, args).spawn() {
            Ok(mut c) => {
                std::thread::spawn(move || {
                    let _ = c.wait();
                });
            }
            Err(err) => tracing::warn!("could not start {name}: {err}"),
        }
    }

    pub fn spawn_component(name: &str) -> std::io::Result<std::process::Child> {
        Self::spawn_component_args(name, &[])
    }

    pub fn spawn_component_args(name: &str, args: &[&str]) -> std::io::Result<std::process::Child> {
        Self::component_command(name, args).spawn()
    }

    /// The command that starts the desktop program `name` (`vesper-bar`, `vesper-launcher` ...): the copy a plugin replaces it with
    /// if one does (run in the plugin's folder), else the one next to this binary, else the one in PATH.
    pub fn component_command(name: &str, args: &[&str]) -> std::process::Command {
        let replaced = REPLACED.lock().ok().and_then(|r| r.iter().find(|(n, _, _, _)| n == name).cloned());
        if let Some((_, dir, command, id)) = replaced {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg(format!("{} \"$@\"", vesper_config::plugins::shell_in(&dir, &format!("exec {command}"))))
                .arg(name)
                .args(args)
                .env("VESPER_PLUGIN_ID", id)
                .env("VESPER_PLUGIN_DIR", dir);
            return cmd;
        }
        let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)));
        let bin = sibling.filter(|p| p.is_file()).map(|p| p.into_os_string()).unwrap_or_else(|| name.into());
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args);
        cmd
    }

    /// Start `name`, or close it if the copy in `slot` is still running.
    pub(crate) fn toggle_child(slot: &mut Option<std::process::Child>, name: &str) {
        if let Some(child) = slot.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
                *slot = None;
                return;
            }
        }
        match Self::spawn_component(name) {
            Ok(child) => *slot = Some(child),
            Err(err) => tracing::warn!("could not start {name}: {err}"),
        }
    }

    /// Start the file manager (Mod+E, or the start menu).
    pub fn open_files(&self) {
        let fm = if self.config.apps.file_manager.trim().is_empty() { "file-manager" } else { self.config.apps.file_manager.as_str() };
        match Self::spawn_component(fm) {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::warn!("could not start the file manager: {err}"),
        }
    }

    /// Is the window switcher on screen? (Mod+Tab then goes to it, to pick the next window.)
    pub fn switcher_running(&mut self) -> bool {
        let running = match self.switcher.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        };
        if !running {
            self.switcher = None;
        }
        running
    }

    /// Open the launcher, or close it if it is already open.
    pub fn toggle_launcher(&mut self) {
        Self::toggle_child(&mut self.launcher, "vesper-launcher");
    }

    /// Open the wallpaper changer, or close it if it is already open.
    pub fn toggle_wallpaper_picker(&mut self) {
        Self::toggle_child(&mut self.wallpaper_picker, "vesper-wallpaper-picker");
    }

    pub fn spawn(&self, command: &str) {
        tracing::info!("spawning: {command}");
        match std::process::Command::new("sh").arg("-c").arg(command).spawn() {
            Ok(mut child) => {
                // Reap it so we never leave zombies behind.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::error!("failed to spawn {command:?}: {err}"),
        }
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        match reason {
            DisconnectReason::ConnectionClosed => tracing::debug!("client {client_id:?} disconnected (connection closed)"),
            DisconnectReason::ProtocolError(e) => tracing::warn!(
                "client {client_id:?} disconnected by protocol error on {} #{} (code {}): {}",
                e.object_interface, e.object_id, e.code, e.message
            ),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum WinMode {
    Tiled,
    Fullscreen,
    Floating,
}

/// Tell a window its size and whether it is tiled, floating or fullscreen.
fn configure(w: &Window, at: Point<i32, Logical>, width: i32, height: i32, mode: WinMode) {
    if let Some(x) = w.x11_surface() {
        // an X11 window learns its place too: its menus and popups position themselves from it
        let want = Rectangle::new(at, (width, height).into());
        if !x.is_override_redirect() {
            if x.is_fullscreen() != (mode == WinMode::Fullscreen) {
                let _ = x.set_fullscreen(mode == WinMode::Fullscreen);
            }
            if x.geometry() != want {
                let _ = x.configure(want);
            }
        }
        return;
    }
    let Some(top) = w.toplevel() else { return };
    top.with_pending_state(|s| {
        s.size = Some((width, height).into());
        for st in [
            xdg_toplevel::State::TiledLeft,
            xdg_toplevel::State::TiledRight,
            xdg_toplevel::State::TiledTop,
            xdg_toplevel::State::TiledBottom,
        ] {
            if mode == WinMode::Tiled {
                s.states.set(st);
            } else {
                s.states.unset(st);
            }
        }
        if mode == WinMode::Fullscreen {
            s.states.set(xdg_toplevel::State::Fullscreen);
        } else {
            s.states.unset(xdg_toplevel::State::Fullscreen);
        }
    });
    top.send_pending_configure();
}

impl Vesper {
    /// Re-read the push-to-talk setting: which key it is, if it is on.
    pub fn ptt_refresh(&mut self) {
        use smithay::input::keyboard::xkb;
        let m = &self.config.media;
        // "code:190" names a physical key (evdev code), anything else is an xkb key name
        let by_code = m.ptt_key.trim().strip_prefix("code:").and_then(|n| n.trim().parse::<u32>().ok()).map(|n| n + 8);
        self.ptt_code = if m.ptt { by_code } else { None };
        self.ptt_sym = if m.ptt && by_code.is_none() {
            Some(xkb::keysym_from_name(&m.ptt_key, xkb::KEYSYM_NO_FLAGS).raw()).filter(|k| *k != 0)
        } else {
            None
        };
        if self.ptt_sym.is_some() || self.ptt_code.is_some() {
            tracing::info!("push to talk on: key {:?}", m.ptt_key);
        }
        if m.ptt && self.ptt_sym.is_none() && self.ptt_code.is_none() {
            tracing::warn!("push to talk: unknown key {:?}", m.ptt_key);
        }
    }

    /// Mute or unmute the default microphone (waits for it, so a quick press and release stay in order).
    pub fn ptt_set_mic(muted: bool) {
        let out = std::process::Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", if muted { "1" } else { "0" }])
            .stdin(std::process::Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => tracing::info!("push to talk: microphone {}", if muted { "muted" } else { "live" }),
            Ok(o) => tracing::warn!("push to talk: wpctl failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")),
            Err(err) => tracing::warn!("push to talk: could not run wpctl: {err}"),
        }
    }

    /// Let go of push to talk (the session paused, the screen locked, ...): the microphone must not stay live.
    pub fn ptt_release(&mut self) {
        if (self.ptt_sym.is_some() || self.ptt_code.is_some()) && self.ptt_held {
            self.ptt_held = false;
            Self::ptt_set_mic(true);
        }
    }

    /// Once a second: for the first half minute of the session keep the microphone muted while push to talk is on
    /// (the sound server may not be up yet when we start).
    pub fn ptt_tick(&mut self) {
        if (self.ptt_sym.is_some() || self.ptt_code.is_some()) && !self.ptt_held && self.ptt_boot < 30 {
            if self.ptt_boot % 3 == 0 {
                Self::ptt_set_mic(true);
            }
            self.ptt_boot += 1;
        }
    }
}

#[cfg(test)]
mod ptt_tests {
    use smithay::input::keyboard::{keysyms, xkb};

    #[test]
    fn mic_button_name_resolves() {
        let k = xkb::keysym_from_name("XF86AudioMicMute", xkb::KEYSYM_NO_FLAGS);
        assert_eq!(k.raw(), keysyms::KEY_XF86AudioMicMute);
        assert_eq!(xkb::keysym_from_name("F4", xkb::KEYSYM_NO_FLAGS).raw(), keysyms::KEY_F4);
        assert_eq!(xkb::keysym_from_name("NotAKey", xkb::KEYSYM_NO_FLAGS).raw(), 0);
    }
}

impl Vesper {
    /// Open the launcher in its clipboard mode (or close it if it is open).
    pub fn toggle_clipboard_picker(&mut self) {
        self.toggle_launcher_mode("clipboard");
    }

    /// Open the launcher in the given mode ("clipboard", "emoji"), or close it if it is open.
    pub fn toggle_launcher_mode(&mut self, mode: &str) {
        if let Some(child) = self.launcher.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
                self.launcher = None;
                return;
            }
        }
        match Self::spawn_component_args("vesper-launcher", &["--mode", mode]) {
            Ok(child) => self.launcher = Some(child),
            Err(err) => tracing::warn!("could not start the clipboard picker: {err}"),
        }
    }

    /// Something was copied: remember its text (a short history in the runtime directory, readable only by you).
    pub fn remember_clipboard(&mut self, mimes: Vec<String>) {
        use std::io::Read;
        use std::os::fd::OwnedFd;
        if !self.config.shell.clipboard_history {
            return;
        }
        let Some(mime) = mimes.iter().find(|m| matches!(m.as_str(), "text/plain;charset=utf-8" | "text/plain" | "UTF8_STRING")).cloned() else { return };
        let Ok((reader, writer)) = std::os::unix::net::UnixStream::pair() else { return };
        if smithay::wayland::selection::data_device::request_data_device_client_selection(&self.seat, mime, OwnedFd::from(writer)).is_err() {
            return;
        }
        std::thread::spawn(move || {
            let _ = reader.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buf = Vec::new();
            let _ = reader.take(64 * 1024).read_to_end(&mut buf);
            let text = String::from_utf8_lossy(&buf).trim().to_string();
            if text.is_empty() {
                return;
            }
            let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| "/tmp".into());
            let path = dir.join("vesper-clipboard.json");
            let mut list: Vec<String> = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            list.retain(|t| *t != text);
            list.insert(0, text);
            list.truncate(25);
            if let Ok(json) = serde_json::to_string(&list) {
                use std::os::unix::fs::OpenOptionsExt;
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path) {
                    let _ = std::io::Write::write_all(&mut f, json.as_bytes());
                }
            }
        });
    }
}

/// Wait for a child in a background thread so it never lingers as a zombie.
/// Programs that enabled plugins replace: (program name, plugin folder, command, plugin id). Set by `plugins_sync`.
static REPLACED: std::sync::Mutex<Vec<(String, std::path::PathBuf, String, String)>> = std::sync::Mutex::new(Vec::new());

/// Remember which programs plugins replace (called whenever the plugins are looked at again).
pub fn set_replacements(list: Vec<(String, std::path::PathBuf, String, String)>) {
    if let Ok(mut r) = REPLACED.lock() {
        *r = list;
    }
}

pub fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Frames per output since the last summary line in the log.
#[derive(Default, Clone, Copy)]
pub struct RenderStats {
    pub queued: u32,
    pub empty: u32,
    pub errors: u32,
    pub elements: usize,
}

impl Vesper {
    /// Write the outputs, their layer surfaces (bar, wallpaper, popups) and the window count to the log. Called when a
    /// layer surface or window appears or goes, so the log shows what the compositor thought the scene looked like.
    pub fn dump_scene(&self, why: &str) {
        let mut out = format!("scene ({why}): primary={:?}", self.output().map(|o| o.name()));
        for o in self.space.outputs() {
            let geo = self.space.output_geometry(o);
            out += &format!("\n  output {} geo={:?} scale={}", o.name(), geo.map(|g| (g.loc.x, g.loc.y, g.size.w, g.size.h)), o.current_scale().fractional_scale());
            let map = layer_map_for_output(o);
            for l in map.layers() {
                let g = map.layer_geometry(l).map(|g| (g.loc.x, g.loc.y, g.size.w, g.size.h));
                out += &format!("\n    layer {:?} {:?} geo={:?}", l.namespace(), l.layer(), g);
            }
        }
        let wins = self.space.elements().count();
        out += &format!("\n  windows mapped={wins} workspace={} focus={:?}", self.layout.active, self.layout.focused_window().map(|w| crate::effects::app_id(w)));
        for w in self.space.elements() {
            let g = self.space.element_geometry(w).map(|g| (g.loc.x, g.loc.y, g.size.w, g.size.h));
            out += &format!("\n    window {:?} {} geo={:?}", crate::effects::app_id(w), if self.layout.is_floating(w) { "floating" } else { "tiled" }, g);
        }
        for (i, ws) in self.layout.workspaces.iter().enumerate().filter(|(i, ws)| *i == self.layout.active || !ws.is_empty()) {
            out += &format!("\n  workspace {i}: float_mode={} columns={} floating={} forced={}", ws.float_mode, ws.columns.len(), ws.floating.len(), ws.float_forced.len());
        }
        tracing::info!("{out}");
    }
}

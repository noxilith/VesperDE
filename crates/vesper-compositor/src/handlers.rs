use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, PopupKeyboardGrab, PopupKind, PopupPointerGrab,
        PopupUngrabStrategy,
    },
    input::{
        pointer::{CursorImageStatus, Focus},
        Seat, SeatHandler, SeatState,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            protocol::{wl_buffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, is_sync_subsurface, with_states, CompositorClientState, CompositorHandler, CompositorState},
        output::OutputHandler,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::xdg::{
            PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::state::{ClientState, Vesper};

impl CompositorHandler for Vesper {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(x) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &x.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if self.handle_layer_commit(surface) {
            return;
        }
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.layout.window_for_surface(&root) {
                window.on_commit();
            }
        }
        self.popups.commit(surface);
        self.handle_toplevel_commit(surface);
        self.apply_window_rules(surface);
        self.ipc_surface_committed(surface); // the title or app id may have changed
        if let Some(popup) = self.popups.find_popup(surface) {
            if let PopupKind::Xdg(xdg) = &popup {
                if !xdg.is_initial_configure_sent() {
                    let _ = xdg.send_configure();
                }
            }
        }
    }
}

impl Vesper {
    fn handle_toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(window) = self.layout.window_for_surface(surface) else { return };
        let Some(top) = window.toplevel() else { return };
        let sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });
        if !sent {
            top.send_configure();
        }

        // On a window's very first commit its parent and size limits are known: dialogs and fixed-size
        // windows float instead of taking a column.
        struct Decided;
        struct ParentFloated;
        let window = window.clone();
        if window.user_data().insert_if_missing(|| Decided) {
            if let Some(rect) = self.float_rect_for(surface) {
                self.layout.float_window(&window, rect);
                self.layout_changed();
            }
        }
        // Announce the window to screen-sharing clients (the picker lists it) and keep its title current.
        if self.extcapture.is_some() {
            let (title, app_id) = with_states(surface, |s| {
                let d = s.data_map.get::<XdgToplevelSurfaceData>().unwrap().lock().unwrap();
                (d.title.clone().unwrap_or_default(), d.app_id.clone().unwrap_or_default())
            });
            match window.user_data().get::<crate::extcapture::WindowHandle>() {
                None if !title.is_empty() || !app_id.is_empty() => {
                    let handle = self.foreign_toplevel_list.new_toplevel::<Self>(title, app_id);
                    handle.send_done();
                    window.user_data().insert_if_missing(|| crate::extcapture::WindowHandle(handle));
                }
                Some(h) if h.0.title() != title || h.0.app_id() != app_id => {
                    h.0.send_title(&title);
                    h.0.send_app_id(&app_id);
                    h.0.send_done();
                }
                _ => {}
            }
        }
        // A dialog can name its parent (xdg-foreign) after its first commit; float it over that window then.
        if window.toplevel().is_some_and(|t| t.parent().is_some()) && window.user_data().insert_if_missing(|| ParentFloated) {
            tracing::info!("dialog with a parent window: floating it over its parent");
            if !self.layout.is_floating(&window) {
                if let Some(rect) = self.float_rect_for(surface) {
                    self.layout.float_window(&window, rect);
                    self.layout_changed();
                }
            }
        }
    }

    /// If a freshly mapped toplevel should float (it has a parent, or a fixed size), where to put it.
    fn float_rect_for(&self, surface: &WlSurface) -> Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>> {
        let top = self.layout.window_for_surface(surface)?.toplevel()?;
        let (min, max) = with_states(surface, |s| {
            let mut c = s.cached_state.get::<SurfaceCachedState>();
            let c = c.current();
            (c.min_size, c.max_size)
        });
        // The settings window is a roomy floating window, not a half-screen column.
        let app_id = with_states(surface, |s| s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().unwrap().app_id.clone()));
        if app_id.as_deref() == Some("vesper-settings") {
            let out = self.output_geometry()?;
            let size: smithay::utils::Size<i32, smithay::utils::Logical> = ((out.size.w * 9 / 10).min(1100), (out.size.h * 9 / 10).min(760)).into();
            let loc = (out.loc.x + (out.size.w - size.w) / 2, out.loc.y + (out.size.h - size.h) / 2);
            return Some(smithay::utils::Rectangle::new(loc.into(), size));
        }
        let fixed = min.w > 0 && min.h > 0 && min == max;
        if top.parent().is_none() && !fixed {
            return None;
        }
        let mut rect = self.default_float_rect();
        let out = self.output_geometry()?;
        // A dialog floats centred over the window that asked for it (file chooser, etc.), not the screen.
        let parent_geo = top
            .parent()
            .and_then(|p| self.layout.window_for_surface(&p).cloned())
            .and_then(|w| self.space.element_geometry(&w))
            .filter(|g| g.size.w > 0 && g.size.h > 0);
        let size: smithay::utils::Size<i32, smithay::utils::Logical> = if fixed {
            min
        } else if let Some(g) = parent_geo {
            ((g.size.w * 85 / 100).clamp(480.min(out.size.w), out.size.w), (g.size.h * 85 / 100).clamp(360.min(out.size.h), out.size.h)).into()
        } else {
            (rect.size.w * 4 / 5, rect.size.h * 4 / 5).into()
        };
        let centre = parent_geo.map(|g| (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2)).unwrap_or((out.loc.x + out.size.w / 2, out.loc.y + out.size.h / 2));
        let x = (centre.0 - size.w / 2).clamp(out.loc.x, (out.loc.x + out.size.w - size.w).max(out.loc.x));
        let y = (centre.1 - size.h / 2).clamp(out.loc.y, (out.loc.y + out.size.h - size.h).max(out.loc.y));
        rect.loc = (x, y).into();
        rect.size = size;
        Some(rect)
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(window) = self.layout.window_for_surface(&root) else { return };
        let (Some(geo), Some(window_geo)) = (self.output_geometry(), self.space.element_geometry(window)) else {
            return;
        };
        let mut target = geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;
        popup.with_pending_state(|s| s.geometry = s.positioner.get_unconstrained_geometry(target));
    }
}

impl BufferHandler for Vesper {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Vesper {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for Vesper {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("new toplevel");
        let window = smithay::desktop::Window::new_wayland_window(surface);
        self.add_window_here(window);
        self.layout_changed();
        self.dump_scene("new toplevel");
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.layout.window_for_surface(surface.wl_surface()).cloned() {
            if let Some(h) = w.user_data().get::<crate::extcapture::WindowHandle>() {
                self.foreign_toplevel_list.remove_toplevel(&h.0);
            }
            self.fx.forget(&w);
        }
        if self.layout.remove(surface.wl_surface()) {
            self.layout_changed();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<wl_output::WlOutput>) {
        if let Some((w, c)) = self.layout.find(surface.wl_surface()) {
            let window = self.layout.workspaces[w].columns[c].clone();
            self.layout.focus_window(&window);
            self.layout.ws_mut().fullscreen = Some(window);
            self.layout_changed();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some((w, _)) = self.layout.find(surface.wl_surface()) {
            if self.layout.workspaces[w].fullscreen.take().is_some() {
                self.layout_changed();
            }
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some((w, c)) = self.layout.find(surface.wl_surface()) {
            let window = self.layout.workspaces[w].columns[c].clone();
            self.layout.focus_window(&window);
            if self.layout.minimize_focused() {
                self.layout_changed();
            }
        }
    }

    // Tiled windows are not moved or resized by hand.
    fn move_request(&mut self, _surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        // A menu or dropdown asked for an explicit grab: it gets the keyboard and pointer until the user
        // clicks outside it or presses Escape, then the compositor dismisses it.
        let Some(seat) = Seat::<Vesper>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else { return };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

impl SeatHandler for Vesper {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Vesper> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        smithay::wayland::selection::primary_selection::set_primary_focus(dh, seat, client);
    }
}

impl SelectionHandler for Vesper {
    type SelectionUserData = ();

    fn new_selection(&mut self, ty: smithay::wayland::selection::SelectionTarget, source: Option<smithay::wayland::selection::SelectionSource>, _seat: Seat<Self>) {
        // a Wayland app copied: let X11 apps paste it
        if ty == smithay::wayland::selection::SelectionTarget::Clipboard {
            let mimes = source.map(|s| s.mime_types());
            if let Some(m) = mimes.clone() {
                self.remember_clipboard(m);
            }
            self.announce_clipboard_to_x11(mimes);
        }
    }

    fn send_selection(
        &mut self,
        ty: smithay::wayland::selection::SelectionTarget,
        mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        self.send_clipboard(ty, &mime_type, fd);
    }
}

impl DataDeviceHandler for Vesper {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Vesper {}
impl ServerDndGrabHandler for Vesper {}
impl OutputHandler for Vesper {}

delegate_compositor!(Vesper);
delegate_shm!(Vesper);
delegate_xdg_shell!(Vesper);
delegate_seat!(Vesper);
impl smithay::wayland::tablet_manager::TabletSeatHandler for Vesper {}
smithay::delegate_cursor_shape!(Vesper);
delegate_data_device!(Vesper);
delegate_output!(Vesper);

impl smithay::wayland::shell::xdg::decoration::XdgDecorationHandler for Vesper {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
        // We draw no titlebars, and tiled windows don't need them: ask clients not to draw their own either.
        toplevel.with_pending_state(|s| s.decoration_mode = Some(Mode::ServerSide));
        toplevel.send_configure();
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        self.new_decoration(toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.new_decoration(toplevel);
    }
}
smithay::delegate_xdg_decoration!(Vesper);

impl smithay::wayland::fractional_scale::FractionalScaleHandler for Vesper {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.output().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0);
        smithay::wayland::compositor::with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
        });
    }
}
smithay::delegate_fractional_scale!(Vesper);
smithay::delegate_viewporter!(Vesper);

impl Vesper {
    /// Tell every window the scale of the monitor it is on (after a scale change).
    pub fn send_preferred_scales(&mut self) {
        let wins: Vec<_> = self.space.elements().cloned().collect();
        for w in wins {
            let Some(top) = w.toplevel() else { continue };
            let scale = self
                .space
                .outputs_for_element(&w)
                .first()
                .or_else(|| self.output())
                .map(|o| o.current_scale().fractional_scale())
                .unwrap_or(1.0);
            smithay::wayland::compositor::with_states(top.wl_surface(), |states| {
                smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| fs.set_preferred_scale(scale));
            });
        }
    }
}

impl smithay::wayland::xdg_foreign::XdgForeignHandler for Vesper {
    fn xdg_foreign_state(&mut self) -> &mut smithay::wayland::xdg_foreign::XdgForeignState {
        &mut self.xdg_foreign_state
    }
}
smithay::delegate_xdg_foreign!(Vesper);

impl smithay::wayland::foreign_toplevel_list::ForeignToplevelListHandler for Vesper {
    fn foreign_toplevel_list_state(&mut self) -> &mut smithay::wayland::foreign_toplevel_list::ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}
smithay::delegate_foreign_toplevel_list!(Vesper);

impl smithay::wayland::selection::primary_selection::PrimarySelectionHandler for Vesper {
    fn primary_selection_state(&self) -> &smithay::wayland::selection::primary_selection::PrimarySelectionState {
        &self.primary_selection_state
    }
}
smithay::delegate_primary_selection!(Vesper);

impl smithay::wayland::xdg_activation::XdgActivationHandler for Vesper {
    fn activation_state(&mut self) -> &mut smithay::wayland::xdg_activation::XdgActivationState {
        &mut self.xdg_activation_state
    }

    /// An app asks for attention (a link opened in the browser, a notification click): bring its window forward.
    fn request_activation(
        &mut self,
        _token: smithay::wayland::xdg_activation::XdgActivationToken,
        _data: smithay::wayland::xdg_activation::XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(w) = self.layout.window_for_surface(&surface).cloned() {
            self.layout.focus_window(&w);
            self.focus_window(Some(w));
            self.layout_changed();
        }
    }
}
smithay::delegate_xdg_activation!(Vesper);

// Apps may set a window icon; the shell draws none of them, so the request is accepted and ignored.
impl smithay::wayland::xdg_toplevel_icon::XdgToplevelIconHandler for Vesper {}
smithay::delegate_xdg_toplevel_icon!(Vesper);

smithay::delegate_text_input_manager!(Vesper);

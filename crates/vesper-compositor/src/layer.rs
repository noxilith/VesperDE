//! wlr-layer-shell: bars, docks, wallpapers, launchers and lock screens draw through this.

use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::{wl_output, wl_surface::WlSurface},
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceCachedState,
            LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};

use crate::state::Vesper;

impl WlrLayerShellHandler for Vesper {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // a namespace like `vesper-bar@HDMI-A-1` asks for that monitor by name (for clients that cannot look the output up)
        let named = namespace.split_once('@').and_then(|(_, name)| self.space.outputs().find(|o| o.name() == name).cloned());
        // no monitor asked for: the bar, its popups, notifications and the like belong on the main monitor; the launcher,
        // power menu and other things you call up open where you are (the monitor under the pointer)
        let follows_focus = matches!(namespace.as_str(), "vesper-launcher" | "vesper-launcher-backdrop" | "vesper-power" | "vesper-switcher" | "vesper-wallpaper-picker" | "vesper-polkit");
        let fallback = if follows_focus { self.focused_output() } else { self.output() }.cloned();
        let Some(output) = output.as_ref().and_then(Output::from_resource).or(named).or(fallback) else {
            surface.send_close();
            return;
        };
        tracing::info!("new layer surface {namespace:?} on {}", output.name());
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("map_layer: {err}");
        }
        drop(map);
        self.dump_scene("layer surface added");
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            let mut map = layer_map_for_output(&output);
            let found = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(layer) = found {
                tracing::info!("layer surface {:?} gone from {}", layer.namespace(), output.name());
                map.unmap_layer(&layer);
                drop(map);
                self.layout_changed();
                self.dump_scene("layer surface gone");
                // give the keyboard back to the focused window (focus changes were held back while the layer had it)
                let f = self.layout.focused_window().cloned();
                self.focus_window(f);
                return;
            }
        }
    }
}

delegate_layer_shell!(Vesper);

impl Vesper {
    /// Does `surface` (or its root) belong to a layer-shell client such as the bar or a launcher?
    pub fn is_layer_surface(&self, surface: &WlSurface) -> bool {
        self.space.outputs().any(|o| layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::ALL).is_some())
    }

    /// Is the keyboard held by a layer surface (the launcher)? Then windows must not pull focus away from it.
    pub fn keyboard_on_layer(&self) -> bool {
        self.seat.get_keyboard().and_then(|k| k.current_focus()).is_some_and(|s| self.is_layer_surface(&s))
    }
}

impl Vesper {
    /// Called from the compositor commit handler. Returns true if `surface` was a layer surface.
    pub fn handle_layer_commit(&mut self, surface: &WlSurface) -> bool {
        let Some(output) = self
            .space
            .outputs()
            .find(|o| layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).is_some())
            .cloned()
        else {
            return false;
        };

        let initial_sent = with_states(surface, |states| {
            states.data_map.get::<LayerSurfaceData>().unwrap().lock().unwrap().initial_configure_sent
        });

        let (layer, interactive) = {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            let layer = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).unwrap().clone();
            if !initial_sent {
                layer.layer_surface().send_configure();
            }
            let exclusive = with_states(surface, |s| {
                s.cached_state.get::<LayerSurfaceCachedState>().current().keyboard_interactivity
                    == KeyboardInteractivity::Exclusive
            });
            (layer.layer(), exclusive)
        };

        // Exclusive launchers / lock screens grab the keyboard when they first appear.
        if !initial_sent && interactive && matches!(layer, Layer::Top | Layer::Overlay) {
            if let Some(kb) = self.seat.get_keyboard() {
                kb.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
            }
        }

        // The exclusive zone may have changed, so re-tile.
        self.relayout();
        if !initial_sent {
            self.dump_scene("layer surface first commit");
        }
        true
    }

    /// The surface under `pos`, looking at overlay/top layers, then windows, then bottom/background layers.
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        // the monitor under the pointer (its bars, docks and the lock screen), else the primary one
        let output = self.space.outputs().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(pos))).or_else(|| self.output())?;
        if let Some(surfaces) = self.lock_surfaces_for(output) {
            let origin = self.space.output_geometry(output)?.loc.to_f64();
            return surfaces.into_iter().next().map(|s| (s, origin));
        }
        let out_geo = self.space.output_geometry(output)?;
        let local = pos - out_geo.loc.to_f64();
        let map = layer_map_for_output(output);

        let from_layers = |layers: &[Layer]| {
            for &which in layers {
                for layer in map.layers_on(which).rev() {
                    let Some(geo) = map.layer_geometry(layer) else { continue };
                    if let Some((s, p)) = layer.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                        return Some((s, (p + geo.loc).to_f64() + out_geo.loc.to_f64()));
                    }
                }
            }
            None
        };

        from_layers(&[Layer::Overlay, Layer::Top])
            .or_else(|| {
                self.window_under(pos).and_then(|(window, loc)| {
                    window
                        .surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL)
                        .map(|(s, p)| (s, (p + loc).to_f64()))
                })
            })
            .or_else(|| from_layers(&[Layer::Bottom, Layer::Background]))
    }
}

impl Vesper {
    /// The topmost window under `pos` that belongs to the monitor `pos` is on, with its render location. (A window that
    /// scrolled off the edge of one monitor overlaps the next one in the shared coordinate space but is not shown there,
    /// so it must not catch the pointer there either.)
    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<(smithay::desktop::Window, Point<i32, Logical>)> {
        let out = self.space.outputs().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(pos))).map(|o| o.name());
        let ok = |w: &smithay::desktop::Window| out.as_ref().is_none_or(|n| crate::layout::on_output(w, n));
        if let Some((w, loc)) = self.space.element_under(pos).filter(|(w, _)| ok(w)) {
            return Some((w.clone(), loc));
        }
        self.space.elements().rev().find_map(|w| {
            if !ok(w) {
                return None;
            }
            let loc = self.space.element_location(w)? - w.geometry().loc;
            smithay::desktop::space::SpaceElement::is_in_input_region(w, &(pos - loc.to_f64())).then(|| (w.clone(), loc))
        })
    }
}

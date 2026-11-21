//! Window borders, movement animations, and picking up changes to the config file and the colour palette.

use std::{collections::HashMap, time::{Duration, Instant}};

use smithay::backend::renderer::gles::{element::PixelShaderElement, Uniform};
use smithay::{
    backend::renderer::element::{
        solid::{SolidColorBuffer, SolidColorRenderElement},
        Kind,
    },
    desktop::Window,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
};
use vesper_config::{theme::Palette, Config};

use crate::state::Vesper;

/// One window's border, ready to draw.
#[derive(Clone)]
pub enum BorderEl {
    Pixel(PixelShaderElement),
    Solid(Vec<SolidColorRenderElement>),
}

struct Anim {
    from: Point<i32, Logical>,
    to: Point<i32, Logical>,
    start: Instant,
    /// Take the window off the screen when it arrives (it is leaving a workspace).
    unmap: bool,
}

#[derive(Default)]
pub struct Effects {
    anims: HashMap<Window, Anim>,
    borders: HashMap<Window, [SolidColorBuffer; 4]>,
    /// The drop box element, kept between frames so an unchanged box is not redrawn.
    pub hint: Option<PixelShaderElement>,
    pub pixel_borders: HashMap<Window, (PixelShaderElement, (Rectangle<i32, Logical>, [f32; 4], f32, f32))>,
    pub palette: Palette,
    palette_stamp: Option<std::time::SystemTime>,
    config_stamp: Option<std::time::SystemTime>,
    /// Windows that appear while this is set slide in from this offset (workspace switches).
    pub slide_from: Option<Point<i32, Logical>>,
}

impl Effects {
    pub fn new() -> Self {
        Self {
            palette: Palette::load(),
            palette_stamp: Palette::stamp(),
            config_stamp: Config::stamp(),
            ..Default::default()
        }
    }

    pub fn forget(&mut self, w: &Window) {
        self.anims.remove(w);
        self.borders.remove(w);
    }
}

/// 0..1 -> 0..1 for the configured curve.
pub fn ease(curve: &str, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        "linear" => t,
        "ease-in-out" => {
            if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
        }
        // a slightly overshooting settle
        "spring" => {
            let c = 1.70158 * 0.8;
            1.0 + (c + 1.0) * (t - 1.0).powi(3) + c * (t - 1.0).powi(2)
        }
        _ => 1.0 - (1.0 - t).powi(3),
    }
}

fn rgba(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

impl Vesper {
    fn anim_enabled(&self) -> bool {
        self.config.animations.enabled && self.config.animations.duration_ms > 0
    }

    /// Put a tiled window at `target`, sliding there from wherever it is when animations are on.
    pub fn place(&mut self, w: Window, target: Point<i32, Logical>) {
        // while you drag or resize, everything follows at once (no sliding behind the pointer)
        if self.drag.is_some() {
            self.place_now(w, target);
            return;
        }
        if !self.anim_enabled() {
            self.fx.anims.remove(&w);
            self.space.map_element(w, target, false);
            return;
        }
        match self.space.element_location(&w) {
            Some(cur) => {
                if self.fx.anims.get(&w).is_some_and(|a| a.to == target) {
                    return; // already on its way
                }
                if cur == target {
                    self.fx.anims.remove(&w);
                } else {
                    self.fx.anims.insert(w.clone(), Anim { from: cur, to: target, start: Instant::now(), unmap: false });
                }
                self.space.map_element(w, cur, false);
            }
            None => match self.fx.slide_from {
                Some(off) => {
                    let from = target + off;
                    self.fx.anims.insert(w.clone(), Anim { from, to: target, start: Instant::now(), unmap: false });
                    self.space.map_element(w, from, false);
                }
                None => {
                    self.space.map_element(w, target, false);
                }
            },
        }
    }

    /// Take a window off a workspace that is no longer shown: slide it away during a workspace switch, else just unmap it.
    pub fn retire(&mut self, w: &Window) {
        if self.fx.anims.get(w).is_some_and(|a| a.unmap) {
            return; // already on its way out
        }
        if let (Some(off), true, Some(cur)) = (self.fx.slide_from, self.anim_enabled(), self.space.element_location(w)) {
            self.fx.anims.insert(w.clone(), Anim { from: cur, to: cur - off, start: Instant::now(), unmap: true });
            return;
        }
        self.fx.anims.remove(w);
        self.space.unmap_elem(w);
    }

    /// Run `f` (which changes the active workspace) with the slide direction set from the change.
    pub fn with_workspace_slide(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.layout.active;
        let h = self.output_geometry().map_or(0, |g| g.size.h);
        f(self);
        let after = self.layout.active;
        if after != before {
            self.fx.slide_from = Some((0, if after > before { h } else { -h }).into());
        }
        self.layout_changed();
        self.fx.slide_from = None;
    }

    /// Put a floating window straight at `target` (dragging and resizing must follow the pointer).
    pub fn place_now(&mut self, w: Window, target: Point<i32, Logical>) {
        self.fx.anims.remove(&w);
        self.space.map_element(w, target, false);
    }

    /// Advance running animations. Returns whether any are still running (keep drawing).
    pub fn animate(&mut self) -> bool {
        let a = &self.config.appearance;
        crate::render::set_night_light((a.night_light && !self.night_via_gamma).then_some(a.night_light_strength));
        if self.fx.anims.is_empty() {
            return false;
        }
        let dur = Duration::from_millis(self.config.animations.duration_ms.max(1) as u64);
        let curve = self.config.animations.curve.clone();
        let now = Instant::now();
        let mut moves = Vec::new();
        let mut done = Vec::new();
        self.fx.anims.retain(|w, a| {
            if self.space.element_location(w).is_none() {
                return false; // unmapped meanwhile (closed, minimized, other workspace)
            }
            let t = now.saturating_duration_since(a.start).as_secs_f64() / dur.as_secs_f64();
            let k = ease(&curve, t);
            let x = a.from.x as f64 + (a.to.x - a.from.x) as f64 * k;
            let y = a.from.y as f64 + (a.to.y - a.from.y) as f64 * k;
            moves.push((w.clone(), Point::from((x.round() as i32, y.round() as i32))));
            if t >= 1.0 && a.unmap {
                done.push(w.clone());
            }
            t < 1.0
        });
        for (w, p) in moves {
            if !done.contains(&w) {
                self.space.map_element(w, p, false);
            }
        }
        for w in done {
            self.space.unmap_elem(&w);
        }
        !self.fx.anims.is_empty()
    }

    /// Border rectangles for this output, as render elements (stable ids, so unchanged borders cost nothing).
    pub fn border_elements(&mut self, output: &Output) -> Vec<(Window, BorderEl)> {
        let bw = self.config.appearance.border_width.clamp(0, 64);
        let Some(out_geo) = self.space.output_geometry(output) else { return Vec::new() };
        let scale = Scale::from(output.current_scale().fractional_scale());
        let fullscreen = self.fullscreen_on(output);
        let focused = self.layout.focused_window().cloned();
        if bw == 0 {
            self.fx.borders.clear();
            self.fx.pixel_borders.clear();
            return Vec::new();
        }
        let a = &self.config.appearance;
        let radius = a.corner_radius.clamp(0, 64);
        let pal = &self.fx.palette;
        let focus = vesper_config::theme::hex(&a.border_focus).unwrap_or(pal.primary);
        let unfocus = vesper_config::theme::hex(&a.border_unfocus)
            .unwrap_or_else(|| vesper_config::theme::with_alpha(pal.outline_variant, 0.8));
        let border_prog = self.round.as_ref().map(|r| r.border.clone());

        let windows: Vec<Window> = self.space.elements_for_output(output).cloned().collect();
        self.fx.borders.retain(|w, _| windows.contains(w));
        self.fx.pixel_borders.retain(|w, _| windows.contains(w));
        let mut out = Vec::new();
        for w in windows {
            if fullscreen.as_ref() == Some(&w) {
                continue;
            }
            if crate::xwayland::is_override_redirect(&w) {
                continue; // menus and tooltips get no border
            }
            let floating = self.layout.is_floating(&w);
            if !floating && !a.border_tiled {
                continue;
            }
            let Some(mut r) = self.space.element_geometry(&w) else { continue };
            r.loc -= out_geo.loc;
            let color = rgba(if focused.as_ref() == Some(&w) { focus } else { unfocus });

            // rounded (or any) border from one pixel shader
            if let Some(prog) = &border_prog {
                let area = Rectangle::<i32, Logical>::new((r.loc.x - bw, r.loc.y - bw).into(), (r.size.w + 2 * bw, r.size.h + 2 * bw).into());
                let k = scale.x as f32;
                let key = (area, color, radius as f32 * k, bw as f32 * k);
                let uniforms = || {
                    vec![
                        Uniform::new("radius", key.2),
                        Uniform::new("bw", key.3),
                        Uniform::new("color", (key.1[0], key.1[1], key.1[2], key.1[3])),
                    ]
                };
                let entry = self.fx.pixel_borders.entry(w.clone()).or_insert_with(|| {
                    (PixelShaderElement::new(prog.clone(), area, None, 1.0, uniforms(), Kind::Unspecified), key)
                });
                if entry.1 != key {
                    entry.0.resize(area, None);
                    entry.0.update_uniforms(uniforms());
                    entry.1 = key;
                }
                out.push((w, BorderEl::Pixel(entry.0.clone())));
                continue;
            }

            let bufs = self.fx.borders.entry(w.clone()).or_insert_with(|| std::array::from_fn(|_| SolidColorBuffer::default()));
            let rects: [Rectangle<i32, Logical>; 4] = [
                Rectangle::new((r.loc.x - bw, r.loc.y - bw).into(), (r.size.w + 2 * bw, bw).into()),
                Rectangle::new((r.loc.x - bw, r.loc.y + r.size.h).into(), (r.size.w + 2 * bw, bw).into()),
                Rectangle::new((r.loc.x - bw, r.loc.y).into(), (bw, r.size.h).into()),
                Rectangle::new((r.loc.x + r.size.w, r.loc.y).into(), (bw, r.size.h).into()),
            ];
            let mut els = Vec::new();
            for (buf, rect) in bufs.iter_mut().zip(rects) {
                buf.update(rect.size, color);
                let loc: Point<i32, Physical> = rect.loc.to_physical_precise_round(scale);
                els.push(SolidColorRenderElement::from_buffer(buf, loc, scale, 1.0, Kind::Unspecified));
            }
            out.push((w, BorderEl::Solid(els)));
        }
        out
    }

    /// Every second or so: pick up an edited config file or new palette.
    /// Is any window sliding right now?
    pub fn animating(&self) -> bool {
        !self.fx.anims.is_empty()
    }

    pub fn watch_files(&mut self) {
        let stamp = Palette::stamp();
        if stamp != self.fx.palette_stamp {
            self.fx.palette_stamp = stamp;
            self.fx.palette = Palette::load();
        }
        let stamp = Config::stamp();
        if stamp != self.fx.config_stamp {
            self.fx.config_stamp = stamp;
            self.reload_config();
        }
    }

    /// Apply a changed config file to the running session.
    pub fn reload_config(&mut self) {
        let new = Config::load();
        tracing::info!("config changed, applying");
        let outputs_changed = new.display.outputs != self.config.display.outputs || new.display.primary != self.config.display.primary;
        let inputs_changed = format!("{:?}", new.input) != format!("{:?}", self.config.input);
        let layout_changed = new.layout.workspaces != self.config.layout.workspaces;
        let cursor_was = (self.config.appearance.cursor_theme.clone(), self.config.appearance.cursor_size);
        let ptt_was = (self.config.media.ptt, self.config.media.ptt_key.clone());
        self.config = new;
        if cursor_was != (self.config.appearance.cursor_theme.clone(), self.config.appearance.cursor_size) {
            let a = &self.config.appearance;
            std::env::set_var("XCURSOR_THEME", &a.cursor_theme);
            std::env::set_var("XCURSOR_SIZE", a.cursor_size.to_string());
            crate::cursor::install_for_apps(&a.cursor_theme, a.cursor_size);
        }
        if ptt_was != (self.config.media.ptt, self.config.media.ptt_key.clone()) {
            self.ptt_refresh();
            if self.config.media.ptt != ptt_was.0 {
                self.ptt_held = false;
                Self::ptt_set_mic(self.config.media.ptt); // on: mute now; off: give the microphone back
            }
        }
        self.plugins_sync();
        if layout_changed {
            self.layout.resize_workspaces(self.config.layout.workspaces);
        }
        if inputs_changed {
            self.reconfigure_inputs();
        }
        if outputs_changed {
            self.apply_output_config();
        }
        self.layout_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_ends_at_one_and_starts_at_zero() {
        for c in ["linear", "ease-out", "ease-in-out", "spring", "nonsense"] {
            assert!(ease(c, 0.0).abs() < 1e-9, "{c}");
            assert!((ease(c, 1.0) - 1.0).abs() < 1e-9, "{c}");
        }
        assert!(ease("ease-out", 0.5) > 0.5);
    }
}

impl Vesper {
    /// The windows on `output` that asked for a blurred background.
    /// The window fullscreen on `output`'s current workspace, if any.
    pub fn fullscreen_on(&self, output: &Output) -> Option<Window> {
        self.layout.get(&output.name()).map_or_else(|| self.layout.ws().fullscreen.clone(), |l| l.ws().fullscreen.clone())
    }

    pub fn blur_params(&self, output: &Output) -> crate::blur::BlurParams {
        use crate::blur::{BlurGroup, BlurParams};
        let cfg = &self.config.blur;
        let mut params = BlurParams::default();
        let Some(out_geo) = self.space.output_geometry(output) else { return params };

        // listed applications
        if cfg.enabled && !cfg.apps.is_empty() {
            let fullscreen = self.fullscreen_on(output);
            let mut rects = Vec::new();
            for w in self.space.elements_for_output(output) {
                if fullscreen.as_ref() == Some(w) || !cfg.matches(&crate::effects::app_id(w)) {
                    continue;
                }
                if let Some(mut r) = self.space.element_geometry(w) {
                    r.loc -= out_geo.loc;
                    rects.push(r);
                }
            }
            params.groups.push(BlurGroup { strength: cfg.strength, brightness: cfg.brightness, saturation: cfg.saturation, radius: -1, rects });
        }

        // the top bar, when it is see-through and asked for blur
        let bar = &self.config.bar;
        if bar.blur && bar.opacity < 100 {
            let map = smithay::desktop::layer_map_for_output(output);
            let layer = map.layers().find(|l| l.namespace() == "vesper-bar").and_then(|l| map.layer_geometry(l));
            // blur only what the bar actually draws (a floating bar, a notch or islands are smaller than its surface)
            let rects: Vec<Rectangle<i32, Logical>> = match layer {
                Some(g) if !self.bar_shapes.is_empty() => self
                    .bar_shapes
                    .iter()
                    .map(|s| Rectangle::new((g.loc.x + s[0], g.loc.y + s[1]).into(), (s[2], s[3]).into()))
                    .collect(),
                Some(g) => vec![g],
                None => Vec::new(),
            };
            let radius = if bar.style == "fill" { 0 } else { bar.radius.clamp(0, 40) };
            params.groups.push(BlurGroup { strength: bar.blur_strength, brightness: cfg.brightness, saturation: cfg.saturation, radius, rects });
        }
        params
    }

    /// Mod+B: add or remove the focused window's application from the blur list.
    pub fn toggle_blur_focused(&mut self) {
        let Some(w) = self.layout.focused_window() else { return };
        let id = app_id(w);
        if id.is_empty() {
            return;
        }
        let _ = Config::edit(|t| {
            let blur = t.entry("blur").or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let Some(tbl) = blur.as_table_mut() else { return };
            let list = tbl.entry("apps").or_insert_with(|| toml::Value::Array(Vec::new()));
            if let toml::Value::Array(a) = list {
                if let Some(i) = a.iter().position(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(&id))) {
                    a.remove(i);
                } else {
                    a.push(toml::Value::String(id.clone()));
                }
            }
            tbl.insert("enabled".into(), toml::Value::Boolean(true));
        });
    }
}

/// The application id a window reported (empty until it sets one).
pub fn app_id(w: &Window) -> String {
    use smithay::wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData};
    if let Some(x) = w.x11_surface() {
        return crate::xwayland::x11_app_id(x);
    }
    let Some(top) = w.toplevel() else { return String::new() };
    with_states(top.wl_surface(), |s| {
        s.data_map.get::<XdgToplevelSurfaceData>().and_then(|d| d.lock().unwrap().app_id.clone()).unwrap_or_default()
    })
}

impl Vesper {
    /// If enabled, put the pointer in the middle of the focused window (called after keyboard focus changes only, so
    /// clicking or hovering a window never moves the pointer).
    pub fn warp_pointer_to_focused(&mut self) {
        if !self.config.input.warp_mouse_on_focus || self.drag.is_some() || self.locked() {
            return;
        }
        let Some(w) = self.layout.focused_window().cloned() else { return };
        let Some(geo) = self.space.element_geometry(&w) else { return };
        // the window may still be sliding into place: aim for where it will be
        let loc = self.fx.anims.get(&w).map(|a| a.to).unwrap_or(geo.loc);
        let center = Point::<f64, Logical>::from(((loc.x + geo.size.w / 2) as f64, (loc.y + geo.size.h / 2) as f64));
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.surface_under(center);
        pointer.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location: center,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
    }
}

impl Vesper {
    /// The translucent accent box shown where a dragged tiled window would land, with the dragged window it belongs to.
    pub fn hint_element(&mut self, output: &Output) -> Option<(Window, PixelShaderElement)> {
        let Some(rect) = self.drop_hint else {
            self.fx.hint = None;
            return None;
        };
        let window = self.drag.as_ref().filter(|d| d.tiled && !d.resize)?.window.clone();
        let prog = self.round.as_ref()?.hint.clone();
        let out_geo = self.space.output_geometry(output)?;
        let scale = output.current_scale().fractional_scale() as f32;
        let area = Rectangle::<i32, Logical>::new((rect.loc.x - out_geo.loc.x, rect.loc.y - out_geo.loc.y).into(), rect.size);
        let accent = self.fx.palette.primary;
        let c = |a: f32| (accent[0] as f32 / 255.0, accent[1] as f32 / 255.0, accent[2] as f32 / 255.0, a);
        let uniforms = vec![
            Uniform::new("radius", self.config.appearance.corner_radius.clamp(0, 64) as f32 * scale),
            Uniform::new("bw", 2.0 * scale),
            Uniform::new("fill", c(0.30)),
            Uniform::new("edge", c(0.85)),
        ];
        let el = match self.fx.hint.take() {
            Some(mut el) => {
                el.resize(area, None);
                el.update_uniforms(uniforms);
                el
            }
            None => PixelShaderElement::new(prog, area, None, 1.0, uniforms, Kind::Unspecified),
        };
        self.fx.hint = Some(el.clone());
        Some((window, el))
    }
}

impl Vesper {
    /// The monitor `delta` places to the left (-1) or right (1) of the focused one, ordered by position.
    pub fn monitor_neighbor(&self, delta: isize) -> Option<String> {
        let mut outs: Vec<(i32, i32, String)> = self
            .layout
            .names()
            .into_iter()
            .filter_map(|n| {
                let o = self.space.outputs().find(|o| o.name() == n)?;
                let g = self.space.output_geometry(o)?;
                Some((g.loc.x, g.loc.y, n))
            })
            .collect();
        outs.sort();
        let at = outs.iter().position(|(_, _, n)| n == self.layout.focused_name())? as isize + delta;
        usize::try_from(at).ok().and_then(|i| outs.get(i)).map(|(_, _, n)| n.clone())
    }

    /// Put the pointer in the middle of monitor `name`.
    pub fn warp_pointer_to_output(&mut self, name: &str) {
        let Some(geo) = self.space.outputs().find(|o| o.name() == name).and_then(|o| self.space.output_geometry(o)) else { return };
        let center = Point::<f64, Logical>::from(((geo.loc.x + geo.size.w / 2) as f64, (geo.loc.y + geo.size.h / 2) as f64));
        let pointer = self.seat.get_pointer().unwrap();
        let under = self.surface_under(center);
        pointer.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location: center,
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: self.start_time.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
    }

    /// Make monitor `name` the focused one: the pointer moves there and the keyboard goes to its focused window.
    pub fn focus_monitor(&mut self, name: &str) {
        if !self.layout.focus_output(name) {
            return;
        }
        self.warp_pointer_to_output(name);
        let focused = self.layout.focused_window().cloned();
        self.focus_window(focused);
        self.ipc_broadcast();
    }
}

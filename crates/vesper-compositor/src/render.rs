//! Builds the element list for one output: pointer, overlay/top layers, windows, bottom/background layers.

use smithay::backend::renderer::element::Element as ElementTrait;
use smithay::desktop::PopupManager;
use smithay::utils::Size;
use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            memory::MemoryRenderBufferRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
        ImportAll, ImportMem,
    },
    desktop::{
        layer_map_for_output,
        Space, Window,
    },
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    output::Output,
    render_elements,
    utils::{Logical, Physical, Point, Scale},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{compositor::with_states, shell::wlr_layer::Layer},
};

render_elements! {
    pub PointerElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
}

render_elements! {
    pub Element<=GlesRenderer>;
    Pointer = PointerElement<GlesRenderer>,
    Layer = WaylandSurfaceRenderElement<GlesRenderer>,
    Border = SolidColorRenderElement,
    Rounded = crate::round::RoundedSurface,
    Pixel = smithay::backend::renderer::gles::element::PixelShaderElement,
    Shaded = smithay::backend::renderer::gles::element::TextureShaderElement,
    Blur = smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
}

/// Is a window fullscreen on `output`? It then covers the bars (the top layer): they are neither drawn nor given the pointer.
pub fn has_fullscreen(space: &Space<Window>, output: &Output) -> bool {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    space.elements_for_output(output).filter(|w| crate::layout::on_output(w, &output.name())).any(|w| {
        if let Some(x) = w.x11_surface() {
            return x.is_fullscreen() && !x.is_override_redirect();
        }
        w.toplevel().is_some_and(|t| t.with_pending_state(|s| s.states.contains(State::Fullscreen)))
    })
}

fn layer_elements(renderer: &mut GlesRenderer, output: &Output, layers: &[Layer], out: &mut Vec<Element>) {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let map = layer_map_for_output(output);
    for &which in layers {
        // Later-mapped surfaces are on top, and the first element is the topmost.
        for layer in map.layers_on(which).rev() {
            let Some(geo) = map.layer_geometry(layer) else { continue };
            let loc: Point<i32, Physical> = geo.loc.to_physical_precise_round(scale);
            out.extend(
                layer
                    .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, loc, scale, 1.0)
                    .into_iter()
                    .map(Element::from),
            );
        }
    }
}

/// `pointer`: logical pointer position and the cursor image to draw (None on backends where the host draws it).
pub fn output_elements(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    pointer: Option<(Point<f64, Logical>, &CursorImageStatus, &crate::cursor::CursorSet)>,
    borders: Vec<(Window, crate::effects::BorderEl)>,
    hint: Option<(Window, smithay::backend::renderer::gles::element::PixelShaderElement)>,
    blur: Option<(&mut crate::blur::BlurState, &crate::blur::BlurParams)>,
    style: Option<(&crate::round::RoundShaders, i32)>,
    lock: Option<&[WlSurface]>,
) -> Vec<Element> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut elements: Vec<Element> = Vec::new();

    // Locked: show only the lock client's surfaces, nothing else.
    if let Some(surfaces) = lock {
        for s in surfaces {
            let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(renderer, s, (0, 0), scale, 1.0, Kind::Unspecified);
            elements.extend(els.into_iter().map(Element::from));
        }
        return elements;
    }

    if let (Some((loc, status, cursors)), Some(out_geo)) = (pointer, space.output_geometry(output)) {
        let pos: Point<f64, Physical> = (loc - out_geo.loc.to_f64()).to_physical(scale);
        match status {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Named(icon) => {
                let (buf, hot) = cursors.get(*icon);
                let tip: Point<f64, Physical> = Point::from((pos.x - hot.0 as f64 * scale.x, pos.y - hot.1 as f64 * scale.y));
                if let Ok(el) =
                    MemoryRenderBufferRenderElement::from_buffer(renderer, tip, buf, None, None, None, Kind::Cursor)
                {
                    elements.push(PointerElement::from(el).into());
                }
            }
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<CursorImageAttributes>>()
                        .map(|a| a.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let at: Point<i32, Physical> = (pos - hotspot.to_f64().to_physical(scale)).to_i32_round();
                let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, surface, at, scale, 1.0, Kind::Cursor);
                elements.extend(els.into_iter().map(|e| Element::from(PointerElement::from(e))));
            }
        }
    }

    // a fullscreen window covers the bar: only what is above everything (launcher, notifications, popups) stays over it
    let above: &[Layer] = if has_fullscreen(space, output) { &[Layer::Overlay] } else { &[Layer::Overlay, Layer::Top] };
    layer_elements(renderer, output, above, &mut elements);
    // Windows, topmost first; each window's border sits just under it (so a window above covers the borders below).
    if let Some(out_geo) = space.output_geometry(output) {
        let mut wins: Vec<Window> = space.elements_for_output(output).filter(|w| crate::layout::on_output(w, &output.name())).cloned().collect();
        wins.reverse();
        let radius = style.map_or(0.0, |(_, r)| r as f32 * scale.x as f32);
        for w in wins {
            let Some(loc) = space.element_location(&w) else { continue };
            let at: Point<i32, Physical> = (loc - out_geo.loc - w.geometry().loc).to_physical_precise_round(scale);
            let win_phys: Size<i32, Physical> = w.geometry().size.to_physical_precise_round(scale);
            if let Some(surface) = smithay::wayland::seat::WaylandFocus::wl_surface(&w).map(|s| s.into_owned()) {
                let surface = &surface;
                let or = crate::xwayland::is_override_redirect(&w);
                for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                    let offset = (w.geometry().loc + popup_offset - popup.geometry().loc).to_physical_precise_round(scale);
                    let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                        render_elements_from_surface_tree(renderer, popup.wl_surface(), at + offset, scale, 1.0, Kind::Unspecified);
                    elements.extend(els.into_iter().map(Element::from));
                }
                let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    render_elements_from_surface_tree(renderer, surface, at, scale, 1.0, Kind::Unspecified);
                // Round the window's surfaces; popups stay square. The mask follows the window's
                // own rectangle, so apps that draw a shadow or their own corners around it (Firefox, GTK) get ours exactly.
                let geo_loc: Point<i32, Physical> = w.geometry().loc.to_physical_precise_round(scale);
                for e in els {
                    match (style, radius > 0.0 && !or) {
                        (Some((shaders, _)), true) => {
                            // every surface of the window is clipped to the window rectangle (some apps, like Firefox, build the
                            // window from several surfaces)
                            let g = e.geometry(scale);
                            let geo = ((at.x + geo_loc.x - g.loc.x) as f32, (at.y + geo_loc.y - g.loc.y) as f32, win_phys.w as f32, win_phys.h as f32);
                            elements.push(Element::from(crate::round::RoundedSurface::new(e, shaders.tex.clone(), radius, geo)));
                        }
                        _ => elements.push(Element::from(e)),
                    }
                }
            }
            // the drop box sits just under the window being dragged (and above everything else)
            if let Some((_, h)) = hint.as_ref().filter(|(hw, _)| *hw == w) {
                elements.push(Element::from(h.clone()));
            }
            if let Some((_, b)) = borders.iter().find(|(bw, _)| *bw == w) {
                match b {
                    crate::effects::BorderEl::Pixel(p) => elements.push(Element::from(p.clone())),
                    crate::effects::BorderEl::Solid(s) => elements.extend(s.iter().cloned().map(Element::from)),
                }
            }
        }
    }
    let mut bg: Vec<Element> = Vec::new();
    layer_elements(renderer, output, &[Layer::Bottom, Layer::Background], &mut bg);
    if let (Some((state, params)), Some(mode)) = (blur, crate::outputs::upright_size(output)) {
        let els = state.elements(renderer, &bg, mode, scale, params, style);
        elements.extend(els);
    }
    elements.extend(bg);
    // night light: a warm veil over everything, topmost
    let alpha = NIGHT_ALPHA.load(std::sync::atomic::Ordering::Relaxed);
    if alpha > 0 {
        if let Some(mode) = crate::outputs::upright_size(output) {
            let a = alpha as f32 / 255.0;
            let color = [1.0 * a, 0.55 * a, 0.15 * a, a];
            let size: Size<i32, Logical> = ((mode.w as f64 / scale.x).ceil() as i32, (mode.h as f64 / scale.y).ceil() as i32).into();
            NIGHT.with(|n| {
                let mut n = n.borrow_mut();
                match n.as_mut() {
                    Some(b) => b.update(size, color),
                    None => *n = Some(SolidColorBuffer::new(size, color)),
                }
                if let Some(b) = n.as_ref() {
                    let el = SolidColorRenderElement::from_buffer(b, (0, 0), scale, 1.0, Kind::Unspecified);
                    elements.insert(0, Element::from(el));
                }
            });
        }
    }
    elements
}

/// 0 = off, otherwise the opacity (0-255) of the night-light veil.
static NIGHT_ALPHA: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

thread_local! {
    static NIGHT: std::cell::RefCell<Option<SolidColorBuffer>> = const { std::cell::RefCell::new(None) };
}

/// Turn the night light on (with a strength of 1 to 100) or off.
pub fn set_night_light(strength: Option<u32>) {
    let a = strength.map_or(0, |s| (s.clamp(1, 100) as f32 * 0.0025 * 255.0).round() as u8);
    NIGHT_ALPHA.store(a, std::sync::atomic::Ordering::Relaxed);
}

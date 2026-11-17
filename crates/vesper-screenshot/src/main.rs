//! vesper-screenshot: the region picker. The compositor freezes the screen first (it saves the frame and the window
//! rectangles); this overlay shows that frozen frame, so nothing moves while you choose.
//!
//! Hover a window to select it and click to capture it, or drag to capture your own region (dragging overrides the
//! window). Esc or right-click cancels. The chosen rectangle goes back to the compositor, which crops it from the
//! frozen frame.

use std::{io::Write, num::NonZeroU32, os::unix::net::UnixStream, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::client::{
        globals::registry_queue_init,
        protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        Connection, QueueHandle,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use vesper_config::theme::Palette;
use vesper_gfx::{Canvas, Image, Text};
use vesper_ipc::Command;

const BTN_LEFT: u32 = 0x110;
/// Moving further than this (in pixels) turns a click into a drag.
const DRAG_START: f64 = 5.0;

type Rect = (i32, i32, i32, i32);

/// What happens with the area you choose.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// Save a screenshot.
    Shot,
    /// Read the text in it and copy that.
    Text,
    /// Read the QR code in it and copy its content.
    Qr,
}

struct Picker {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Surface size in logical pixels.
    size: (u32, u32),
    /// The frozen frame as straight RGBA, with its size in pixels.
    frozen: Vec<u8>,
    frozen_size: (u32, u32),
    /// The frame scaled to the surface, as it is, and dimmed (both ready to copy onto the canvas).
    bg: Vec<u8>,
    dim: Vec<u8>,
    /// Window rectangles in surface pixels, topmost first.
    windows: Vec<Rect>,
    /// Where the left button went down, once it is down.
    press: Option<(f64, f64)>,
    dragging: bool,
    now: (f64, f64),
    pal: Palette,
    text: Text,
    /// The rectangle to capture, in surface pixels.
    chosen: Option<Rect>,
    mode: Mode,
    done: bool,
}

fn freeze_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    (dir.join("vesper-freeze.rgba"), dir.join("vesper-freeze.json"))
}

/// The frozen frame the compositor saved: (width, height, RGBA) and the window rectangles.
fn load_freeze() -> Result<((u32, u32), Vec<u8>, Vec<Rect>)> {
    let (img, meta) = freeze_paths();
    let data = std::fs::read(&img).with_context(|| format!("reading {} (the compositor freezes the screen first)", img.display()))?;
    if data.len() < 8 {
        return Err(anyhow!("the frozen frame is empty"));
    }
    let w = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let h = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if data.len() != 8 + (w * h * 4) as usize {
        return Err(anyhow!("the frozen frame has the wrong size"));
    }
    let windows: Vec<Rect> = std::fs::read_to_string(&meta)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("windows")?.as_array().map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let r = r.as_array()?;
                        Some((r.first()?.as_i64()? as i32, r.get(1)?.as_i64()? as i32, r.get(2)?.as_i64()? as i32, r.get(3)?.as_i64()? as i32))
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    Ok(((w, h), data[8..].to_vec(), windows))
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = vesper_config::Config::load();
    let (frozen_size, frozen, windows) = load_freeze()?;
    let mode = match std::env::args().skip_while(|a| a != "--mode").nth(1).as_deref() {
        Some("text") => Mode::Text,
        Some("qr") => Mode::Qr,
        _ => Mode::Shot,
    };
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| anyhow!("wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow!("wlr-layer-shell is not available: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow!("wl_shm: {e}"))?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("vesper-screenshot"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let mut app = Picker {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        pool: SlotPool::new(1920 * 1200 * 4, &shm)?,
        shm,
        layer: Some(layer),
        pointer: None,
        keyboard: None,
        size: (0, 0),
        frozen,
        frozen_size,
        bg: Vec::new(),
        dim: Vec::new(),
        windows,
        press: None,
        dragging: false,
        now: (0.0, 0.0),
        pal: Palette::load(),
        text: Text::new(&config.bar.font, 15.0),
        chosen: None,
        mode,
        done: false,
    };
    while !app.done {
        queue.blocking_dispatch(&mut app)?;
    }

    // Take the overlay away, then act on the rectangle.
    app.layer = None;
    let _ = conn.roundtrip();
    drop(queue);
    let (img, meta) = freeze_paths();
    let Some((x, y, w, h)) = app.chosen else {
        let _ = std::fs::remove_file(&img);
        let _ = std::fs::remove_file(&meta);
        return Ok(());
    };
    // surface pixels -> pixels of the frozen frame (they differ on scaled displays)
    let (sw, sh) = (app.size.0.max(1) as f64, app.size.1.max(1) as f64);
    let (fx, fy) = (app.frozen_size.0 as f64 / sw, app.frozen_size.1 as f64 / sh);
    let (px, py) = ((x as f64 * fx).round() as i32, (y as f64 * fy).round() as i32);
    let (pw, ph) = (((w as f64 * fx).round() as i32).max(1), ((h as f64 * fy).round() as i32).max(1));
    let path = vesper_ipc::socket_path();
    let send = |cmd: Command| -> Result<()> {
        let mut sock = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
        writeln!(sock, "{}", serde_json::to_string(&cmd)?)?;
        sock.flush()?;
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    };
    match app.mode {
        Mode::Shot => send(Command::Screenshot { kind: "frozen".into(), x: Some(px), y: Some(py), w: Some(pw), h: Some(ph) })?,
        Mode::Text | Mode::Qr => {
            // read it here, from the frozen frame; the compositor only keeps the clipboard
            let _ = std::fs::remove_file(&img);
            let _ = std::fs::remove_file(&meta);
            let crop = crop_rgba(&app.frozen, app.frozen_size, (px, py, pw, ph));
            let (what, result) = if app.mode == Mode::Text {
                ("Text", ocr(&crop, &config.screenshots.ocr_lang))
            } else {
                ("QR code", decode_qr(&crop))
            };
            let cmd = match result {
                Ok(text) => {
                    let preview: String = text.chars().take(120).collect();
                    Command::Clipboard { text, summary: format!("{what} copied"), body: preview }
                }
                Err(why) => Command::Clipboard { text: String::new(), summary: format!("No {} copied", what.to_lowercase()), body: why },
            };
            send(cmd)?;
        }
    }
    Ok(())
}

/// The part of an RGBA picture `(w, h)` inside `r`, clamped to the picture: (width, height, pixels).
fn crop_rgba(rgba: &[u8], size: (u32, u32), r: Rect) -> (usize, usize, Vec<u8>) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let (x0, y0) = (r.0.clamp(0, w - 1), r.1.clamp(0, h - 1));
    let (x1, y1) = ((r.0 + r.2).clamp(x0 + 1, w), (r.1 + r.3).clamp(y0 + 1, h));
    let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in y0..y1 {
        let s = ((y * w + x0) * 4) as usize;
        out.extend_from_slice(&rgba[s..s + cw * 4]);
    }
    (cw, ch, out)
}

/// Scale an RGBA crop by `k` (bilinear) and return it as RGB.
fn scaled_rgb(crop: &(usize, usize, Vec<u8>), k: usize) -> (usize, usize, Vec<u8>) {
    let (w, h, rgba) = crop;
    let (nw, nh) = (w * k, h * k);
    let img = Image::from_rgba(*w as i32, *h as i32, rgba, false);
    let mut buf = vec![0u8; nw * nh * 4];
    Canvas::new(&mut buf, nw as u32, nh as u32).draw_cover(&img, 0, 0, nw as i32, nh as i32, 0.0, 1.0);
    // the canvas is BGRA
    let rgb: Vec<u8> = buf.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
    (nw, nh, rgb)
}

/// Read the text in a picture with tesseract (the program has to be installed).
fn ocr(crop: &(usize, usize, Vec<u8>), lang: &str) -> std::result::Result<String, String> {
    // small screen text reads better enlarged; tesseract understands PPM files
    let k = if crop.1 < 120 { 3 } else { 2 };
    let (w, h, rgb) = scaled_rgb(crop, k);
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let file = dir.join(format!("vesper-ocr-{}.ppm", std::process::id()));
    let mut data = format!("P6\n{w} {h}\n255\n").into_bytes();
    data.extend_from_slice(&rgb);
    std::fs::write(&file, data).map_err(|e| format!("could not write a temporary picture: {e}"))?;
    let out = std::process::Command::new("tesseract").arg(&file).arg("stdout").args(["-l", lang, "--psm", "6"]).output();
    let _ = std::fs::remove_file(&file);
    match out {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("Install tesseract (and its language data) to copy text from the screen".into()),
        Err(e) => Err(format!("could not run tesseract: {e}")),
        Ok(o) if !o.status.success() => Err(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("tesseract failed").to_string()),
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if text.is_empty() { Err("Nothing readable in that area".into()) } else { Ok(text) }
        }
    }
}

/// Decode the first QR code in a picture.
fn decode_qr(crop: &(usize, usize, Vec<u8>)) -> std::result::Result<String, String> {
    let try_decode = |w: usize, h: usize, gray: &[u8]| -> Option<String> {
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| gray[y * w + x]);
        prepared.detect_grids().into_iter().find_map(|g| g.decode().ok().map(|(_, content)| content))
    };
    for k in [1usize, 2, 3] {
        let (w, h, rgb) = if k == 1 {
            (crop.0, crop.1, crop.2.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect::<Vec<u8>>())
        } else {
            scaled_rgb(crop, k)
        };
        let gray: Vec<u8> = rgb.chunks_exact(3).map(|p| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8).collect();
        if let Some(t) = try_decode(w, h, &gray) {
            return Ok(t);
        }
        // light-on-dark codes
        let inverted: Vec<u8> = gray.iter().map(|v| 255 - v).collect();
        if let Some(t) = try_decode(w, h, &inverted) {
            return Ok(t);
        }
    }
    Err("No QR code found in that area".into())
}

impl Picker {
    /// The window under the pointer, if any.
    fn hovered(&self) -> Option<Rect> {
        let (px, py) = (self.now.0 as i32, self.now.1 as i32);
        self.windows.iter().copied().find(|(x, y, w, h)| px >= *x && px < x + w && py >= *y && py < y + h)
    }

    /// The rectangle being dragged out.
    fn drag_rect(&self) -> Option<Rect> {
        let (sx, sy) = self.press?;
        let (x0, x1) = (sx.min(self.now.0), sx.max(self.now.0));
        let (y0, y1) = (sy.min(self.now.1), sy.max(self.now.1));
        Some((x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32))
    }

    /// What is selected right now: your drag, else the window under the pointer.
    fn selection(&self) -> Option<Rect> {
        if self.dragging {
            self.drag_rect()
        } else {
            self.hovered()
        }
    }

    /// Scale the frozen frame to the surface and prepare the dimmed copy.
    fn build_background(&mut self) {
        let (w, h) = self.size;
        let mut bg = vec![0u8; (w * h * 4) as usize];
        {
            let img = Image::from_rgba(self.frozen_size.0 as i32, self.frozen_size.1 as i32, &self.frozen, false);
            let mut c = Canvas::new(&mut bg, w, h);
            c.draw_cover(&img, 0, 0, w as i32, h as i32, 0.0, 1.0);
        }
        let mut dim = bg.clone();
        for px in dim.chunks_exact_mut(4) {
            // opaque pixels, 60% as bright
            px[0] = (px[0] as u32 * 40 / 100) as u8;
            px[1] = (px[1] as u32 * 40 / 100) as u8;
            px[2] = (px[2] as u32 * 40 / 100) as u8;
        }
        self.bg = bg;
        self.dim = dim;
    }

    fn draw(&mut self) {
        if self.layer.is_none() {
            return;
        }
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        if self.bg.len() != (w * h * 4) as usize {
            self.build_background();
        }
        let Some(layer) = &self.layer else { return };
        let sel = self.selection();
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) else {
            return;
        };
        // the frozen screen, dimmed, with the selection shown at full brightness
        let n = self.dim.len().min(canvas.len());
        canvas[..n].copy_from_slice(&self.dim[..n]);
        let mut c = Canvas::new(canvas, w, h);
        if let Some((x, y, rw, rh)) = sel {
            let (x0, x1) = (x.max(0), (x + rw).min(w as i32));
            for yy in y.max(0)..(y + rh).min(h as i32) {
                let a = ((yy * w as i32 + x0) * 4) as usize;
                let b = ((yy * w as i32 + x1) * 4) as usize;
                if b > a {
                    c.px[a..b].copy_from_slice(&self.bg[a..b]);
                }
            }
            let accent = self.pal.primary;
            let t = 2;
            c.rounded_rect(x - t, y - t, rw + 2 * t, t, 1.0, accent);
            c.rounded_rect(x - t, y + rh, rw + 2 * t, t, 1.0, accent);
            c.rounded_rect(x - t, y, t, rh, 1.0, accent);
            c.rounded_rect(x + rw, y, t, rh, 1.0, accent);
            let label = format!("{} x {}", rw, rh);
            let lw = self.text.width(&label) + 16;
            let ly = if y > 36 { y - 30 } else { y + rh + 8 };
            c.rounded_rect(x, ly, lw, 24, 8.0, self.pal.surface);
            self.text.draw(&mut c, x + 8, ly + 12, &label, self.pal.on_surface);
        }
        if !self.dragging && sel.is_none() {
            let msg = match self.mode {
                Mode::Shot => "Click a window, or drag an area  \u{b7}  Esc to cancel",
                Mode::Text => "Drag over the text to copy  \u{b7}  Esc to cancel",
                Mode::Qr => "Drag over a QR code to read it  \u{b7}  Esc to cancel",
            };
            let mw = self.text.width(msg) + 32;
            let (cx, cy) = (w as i32 / 2, 48);
            c.rounded_rect(cx - mw / 2, cy - 18, mw, 36, 12.0, self.pal.surface);
            self.text.draw_centered(&mut c, cx, cy, msg, self.pal.on_surface);
        }
        layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(layer.wl_surface()).is_ok() {
            layer.commit();
        }
    }
}

impl LayerShellHandler for Picker {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.done = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(c.new_size.0), NonZeroU32::new(c.new_size.1)) {
            let new = (w.get(), h.get());
            if new != self.size {
                self.size = new;
                self.bg.clear(); // rebuilt for the new size
            }
        }
        self.draw();
    }
}

impl PointerHandler for Picker {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for e in events {
            self.now = e.position;
            match e.kind {
                PointerEventKind::Enter { .. } => self.draw(),
                PointerEventKind::Press { button, .. } => {
                    if button == BTN_LEFT {
                        self.press = Some(e.position);
                        self.dragging = false;
                    } else {
                        self.done = true; // any other button cancels
                    }
                }
                PointerEventKind::Motion { .. } => {
                    if let Some((sx, sy)) = self.press {
                        if !self.dragging && ((e.position.0 - sx).abs() > DRAG_START || (e.position.1 - sy).abs() > DRAG_START) {
                            self.dragging = true; // a drag overrides the window selection
                        }
                    }
                    self.draw();
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.chosen = if self.dragging {
                        self.drag_rect().filter(|(_, _, w, h)| *w >= 4 && *h >= 4)
                    } else {
                        // a plain click captures the window it is on
                        self.hovered()
                    };
                    if self.chosen.is_some() {
                        self.done = true;
                    } else {
                        self.press = None;
                        self.dragging = false;
                        self.draw();
                    }
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for Picker {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        if event.keysym == Keysym::Escape {
            self.done = true;
        }
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: RawModifiers, _: u32) {}
}

impl SeatHandler for Picker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        match cap {
            Capability::Pointer if self.pointer.is_none() => self.pointer = self.seat_state.get_pointer(qh, &seat).ok(),
            Capability::Keyboard if self.keyboard.is_none() => self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok(),
            _ => {}
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        match cap {
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
            }
            _ => {}
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl CompositorHandler for Picker {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Picker);
delegate_output!(Picker);
delegate_shm!(Picker);
delegate_seat!(Picker);
delegate_pointer!(Picker);
delegate_keyboard!(Picker);
delegate_layer!(Picker);
delegate_registry!(Picker);

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QR code picture (RGBA) of `text`, `scale` pixels per module with a quiet zone.
    fn qr_picture(text: &str, scale: usize, invert: bool) -> (usize, usize, Vec<u8>) {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let n = code.width();
        let side = (n + 8) * scale;
        let mut px = vec![if invert { 0u8 } else { 255u8 }; side * side * 4];
        for y in 0..side {
            for x in 0..side {
                let (mx, my) = (x / scale, y / scale);
                let dark = mx >= 4 && my >= 4 && mx < n + 4 && my < n + 4 && code[(mx - 4, my - 4)] == qrcode::Color::Dark;
                let v = if dark != invert { 0 } else { 255 };
                let i = (y * side + x) * 4;
                px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        (side, side, px)
    }

    #[test]
    fn reads_qr_codes() {
        assert_eq!(decode_qr(&qr_picture("https://example.org/a?b=1", 6, false)).unwrap(), "https://example.org/a?b=1");
        // small and light-on-dark codes are found too
        assert_eq!(decode_qr(&qr_picture("hello", 3, false)).unwrap(), "hello");
        assert_eq!(decode_qr(&qr_picture("inverted", 5, true)).unwrap(), "inverted");
    }

    #[test]
    fn empty_picture_has_no_qr() {
        let blank = (40, 40, vec![200u8; 40 * 40 * 4]);
        assert!(decode_qr(&blank).is_err());
    }

    #[test]
    fn crop_stays_inside() {
        let img = vec![7u8; 10 * 10 * 4];
        let (w, h, px) = crop_rgba(&img, (10, 10), (8, 8, 50, 50));
        assert_eq!((w, h, px.len()), (2, 2, 16));
    }
}

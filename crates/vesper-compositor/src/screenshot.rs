//! Screenshots: render the output offscreen, crop, encode as PNG on a worker thread, save, copy, notify.

use std::{
    io::Write,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::Arc,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
            Bind, ExportMem, Offscreen, TextureMapping,
        },
    },
    reexports::calloop::channel::Sender,
    utils::{Logical, Physical, Rectangle, Size, Transform},
    wayland::selection::{data_device::set_data_device_selection, SelectionTarget},
};

use crate::{render::Element, state::Vesper};

#[derive(Debug, Clone, Copy)]
pub enum CaptureKind {
    Screen,
    Window,
    Region(Rectangle<i32, Logical>),
    /// Freeze the screen for the region picker: save the frame and the window rectangles, then start the picker.
    Freeze(Pick),
}

/// What the region picker does with the area you choose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pick {
    /// Save a screenshot.
    Shot,
    /// Read the text in it and copy that.
    Text,
    /// Read the QR code in it and copy its content.
    Qr,
}

impl Pick {
    fn arg(self) -> &'static str {
        match self {
            Pick::Shot => "shot",
            Pick::Text => "text",
            Pick::Qr => "qr",
        }
    }
}

/// A finished screenshot, handed back to the compositor thread.
pub struct Done {
    pub png: Arc<Vec<u8>>,
    pub path: Option<PathBuf>,
}

/// Render `elements` into an offscreen texture of `size` and read the pixels back as top-down RGBA.
pub fn grab(renderer: &mut GlesRenderer, elements: &[Element], size: Size<i32, Physical>, scale: f64) -> Option<Vec<u8>> {
    let buf: Size<i32, smithay::utils::Buffer> = (size.w, size.h).into();
    let mut tex: GlesTexture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, buf).ok()?;
    let mut target = renderer.bind(&mut tex).ok()?;
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    tracker.render_output(renderer, &mut target, 0, elements, [0.08, 0.08, 0.1, 1.0]).ok()?;
    let mapping = renderer.copy_framebuffer(&target, Rectangle::from_size(buf), Fourcc::Abgr8888).ok()?;
    // GL reads framebuffers bottom-up; `flipped()` says whether the rows are already top-down.
    let top_down = mapping.flipped();
    let bytes = renderer.map_texture(&mapping).ok()?;
    let row = size.w as usize * 4;
    if top_down {
        return Some(bytes.to_vec());
    }
    let mut out = Vec::with_capacity(bytes.len());
    for y in (0..size.h as usize).rev() {
        out.extend_from_slice(&bytes[y * row..(y + 1) * row]);
    }
    Some(out)
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| p.into()),
        None => p.into(),
    }
}

/// Crop `rgba` (w x h) to `crop` (clamped to the image), encode it, save it into `dir`, and send the result back.
pub fn finish(rgba: Vec<u8>, w: u32, h: u32, crop: Option<Rectangle<i32, Logical>>, dir: String, tx: Sender<Done>) {
    std::thread::spawn(move || {
        let full = Rectangle::<i32, Logical>::from_size((w as i32, h as i32).into());
        let r = crop.and_then(|c| c.intersection(full)).unwrap_or(full);
        if r.size.w <= 0 || r.size.h <= 0 {
            return;
        }
        let (cw, ch) = (r.size.w as usize, r.size.h as usize);
        let mut pixels = Vec::with_capacity(cw * ch * 4);
        for y in 0..ch {
            let start = ((r.loc.y as usize + y) * w as usize + r.loc.x as usize) * 4;
            pixels.extend_from_slice(&rgba[start..start + cw * 4]);
        }
        // the compositor draws opaque; make sure alpha is solid so viewers don't show holes
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 0xff;
        }
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, cw as u32, ch as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_compression(png::Compression::Fast);
            let Ok(mut writer) = enc.write_header() else { return };
            if writer.write_image_data(&pixels).is_err() {
                return;
            }
        }
        let dir = expand_home(&dir);
        let name = chrono::Local::now().format("Screenshot-%Y-%m-%d-%H-%M-%S.png").to_string();
        let path = dir.join(name);
        let saved = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &png_bytes));
        if let Err(err) = &saved {
            tracing::error!("could not save the screenshot to {}: {err}", path.display());
        }
        let _ = tx.send(Done { png: Arc::new(png_bytes), path: saved.is_ok().then_some(path) });
    });
}

/// Where the frozen frame and its window list live while the picker is open.
pub fn freeze_paths() -> (PathBuf, PathBuf) {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    (dir.join("vesper-freeze.rgba"), dir.join("vesper-freeze.json"))
}

/// Save the frozen frame (8 byte header: width, height, then RGBA) and the window rectangles (output-local logical
/// pixels, topmost first), then start the picker, which draws the frame as its background.
pub fn write_freeze(rgba: Vec<u8>, w: u32, h: u32, windows: Vec<[i32; 4]>, pick: Pick, output: &str) {
    let (img, meta) = freeze_paths();
    let mut data = Vec::with_capacity(rgba.len() + 8);
    data.extend_from_slice(&w.to_le_bytes());
    data.extend_from_slice(&h.to_le_bytes());
    data.extend_from_slice(&rgba);
    let json = serde_json::json!({ "windows": windows }).to_string();
    if std::fs::write(&img, data).and_then(|_| std::fs::write(&meta, json)).is_err() {
        tracing::warn!("could not save the frozen screen");
        return;
    }
    Vesper::run_component("vesper-screenshot", &["--mode", pick.arg(), "--output", output]);
}

impl Vesper {
    /// Output-local rectangles of the visible windows, topmost first, for the picker's window hover.
    pub fn freeze_windows(&self) -> Vec<[i32; 4]> {
        let (Some(out), Some(output)) = (self.capture_output().and_then(|o| self.space.output_geometry(o)), self.capture_output()) else { return Vec::new() };
        let mut wins: Vec<_> = self.space.elements_for_output(output).filter(|w| crate::layout::on_output(w, &output.name())).cloned().collect();
        wins.reverse();
        wins.iter()
            .filter_map(|w| self.space.element_geometry(w))
            .map(|g| [g.loc.x - out.loc.x, g.loc.y - out.loc.y, g.size.w, g.size.h])
            .collect()
    }

    /// The picker chose `rect` (in the frozen frame's pixels): crop it from the saved frame, save and copy it.
    pub fn finish_frozen(&mut self, rect: Rectangle<i32, Logical>) {
        let (img, meta) = freeze_paths();
        let data = std::fs::read(&img).unwrap_or_default();
        let _ = std::fs::remove_file(&img);
        let _ = std::fs::remove_file(&meta);
        if data.len() < 8 {
            return;
        }
        let w = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let h = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if data.len() != 8 + (w * h * 4) as usize {
            return;
        }
        finish(data[8..].to_vec(), w, h, Some(rect), self.config.screenshots.dir.clone(), self.shot_tx.clone());
    }

    /// Ask for a screenshot; it is taken when the next frame is rendered.
    pub fn request_capture(&mut self, kind: CaptureKind) {
        if self.locked() {
            return; // never leak the screen behind the lock
        }
        // a picker is already open (its files are fresh): do not start a second one
        if matches!(kind, CaptureKind::Freeze(_)) {
            let (img, _) = freeze_paths();
            let fresh = std::fs::metadata(&img).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() < 120);
            if fresh {
                return;
            }
        }
        self.pending_capture = Some(kind);
        // make sure a frame is rendered soon even if nothing changed
        if let Some(out) = self.output().cloned() {
            let _ = out;
        }
    }

    /// The area to keep, in output-local logical pixels, for the pending capture (None = everything).
    pub fn capture_crop(&self, kind: CaptureKind) -> Option<Rectangle<i32, Logical>> {
        let out = self.capture_output().and_then(|o| self.space.output_geometry(o))?;
        match kind {
            CaptureKind::Screen | CaptureKind::Freeze(_) => None,
            CaptureKind::Region(r) => Some(r),
            CaptureKind::Window => {
                let win = self.layout.focused_window()?;
                let mut g = self.space.element_geometry(win)?;
                g.loc -= out.loc;
                Some(g)
            }
        }
    }

    /// A screenshot is ready: put it on the clipboard and tell the user.
    pub fn screenshot_done(&mut self, done: Done) {
        self.clipboard_png = Some(done.png);
        set_data_device_selection(&self.display_handle, &self.seat, vec!["image/png".into()], ());
        self.announce_clipboard_to_x11(Some(vec!["image/png".into()]));
        let (summary, body) = match &done.path {
            Some(p) => ("Screenshot saved".to_owned(), short_path(p)),
            None => ("Screenshot copied".to_owned(), "Could not save it to disk; it is on the clipboard".to_owned()),
        };
        let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("vesper-notify")));
        let bin = sibling.filter(|p| p.is_file()).map(|p| p.into_os_string()).unwrap_or_else(|| "vesper-notify".into());
        match std::process::Command::new(bin).args(["send", &summary, &body]).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(err) => tracing::debug!("no notification: {err}"),
        }
    }

    /// Serve the screenshot to apps that paste.
    /// Put `text` on the clipboard (if any) and show a notification: the result of the text and QR pickers.
    pub fn copy_text(&mut self, text: String, summary: &str, body: &str) {
        if !text.is_empty() {
            self.clipboard_text = Some(text);
            let mimes = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"];
            set_data_device_selection(&self.display_handle, &self.seat, mimes.iter().map(|m| m.to_string()).collect(), ());
            self.announce_clipboard_to_x11(Some(mimes.iter().map(|m| m.to_string()).collect()));
        }
        Self::run_component("vesper-notify", &["send", summary, body]);
    }

    pub fn send_clipboard(&mut self, ty: SelectionTarget, mime: &str, fd: OwnedFd) {
        // an X11 app owns the clipboard: Xwayland fetches the data for the Wayland app that pastes
        if self.x11_clipboard {
            if let Some(xwm) = self.xwm.as_mut() {
                let _ = xwm.send_selection(ty, mime.to_string(), fd, self.loop_handle.clone());
            }
            return;
        }
        if ty == SelectionTarget::Clipboard {
            self.send_clipboard_own(mime, fd);
        }
    }

    /// Serve the compositor's own clipboard (text from the pickers, screenshots).
    pub fn send_clipboard_own(&mut self, mime: &str, fd: OwnedFd) {
        let ty = SelectionTarget::Clipboard;
        if ty == SelectionTarget::Clipboard && (mime.starts_with("text/") || matches!(mime, "UTF8_STRING" | "TEXT" | "STRING")) {
            if let Some(text) = self.clipboard_text.clone() {
                std::thread::spawn(move || {
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(text.as_bytes());
                });
            }
            return;
        }
        if ty == SelectionTarget::Clipboard && mime == "image/png" {
            if let Some(png) = self.clipboard_png.clone() {
                std::thread::spawn(move || {
                    let mut f = std::fs::File::from(fd);
                    let _ = f.write_all(&png);
                });
            }
        }
    }
}

fn short_path(p: &Path) -> String {
    match (std::env::var_os("HOME"), p.to_str()) {
        (Some(h), Some(s)) => s.replacen(h.to_str().unwrap_or("\0"), "~", 1),
        _ => p.display().to_string(),
    }
}

//! `ext-iSource::Output(o) => crate::outputs::upright_size(o).map(|s| (s.w, s.h))age-copy-capture-v1` with `ext-image-capture-source-v1`: the newer capture protocol that
//! xdg-desktop-portal-wlr (0.8 and later) uses, and the one that can capture a single window. Together with
//! `ext-foreign-toplevel-list-v1` it is what lets the screen-picker of Discord, browsers and OBS offer "a window".
//!
//! Monitors are captured the same way as in [`crate::screencopy`] (shm buffers, the offscreen render the screenshots
//! use); a window is rendered alone into its own buffer. Only shm buffers are offered, like there.

use std::sync::Mutex;

use smithay::{
    backend::renderer::gles::GlesRenderer,
    backend::renderer::element::AsRenderElements,
    desktop::{Space, Window},
    output::Output,
    reexports::{
        wayland_protocols::ext::{
            foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{self, ExtForeignToplevelImageCaptureSourceManagerV1},
                ext_image_capture_source_v1::ExtImageCaptureSourceV1,
                ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
                ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_output, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        },
    },
    utils::{Clock, Monotonic, Physical, Point, Scale, Size},
    wayland::{foreign_toplevel_list::ForeignToplevelHandle, shm::{with_buffer_contents, with_buffer_contents_mut}},
};

use crate::{render::Element, state::Vesper};

const FORMAT: wl_shm::Format = wl_shm::Format::Xrgb8888;

/// What a capture source points at.
#[derive(Clone)]
pub enum Source {
    Output(Output),
    Window(ForeignToplevelHandle),
}

/// The foreign-toplevel handle a window was announced with.
pub struct WindowHandle(pub ForeignToplevelHandle);

/// Per capture session: its source, whether the cursor is wanted, the buffer size announced last, and whether a frame is
/// in flight (there may be only one).
pub struct SessionData {
    source: Source,
    cursor: bool,
    size: Mutex<(i32, i32)>,
    busy: Mutex<bool>,
}

pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    source: Source,
    cursor: bool,
    size: (i32, i32),
    buffer: Mutex<Option<WlBuffer>>,
    captured: Mutex<bool>,
}

struct Pending {
    frame: ExtImageCopyCaptureFrameV1,
    session: ExtImageCopyCaptureSessionV1,
    source: Source,
    cursor: bool,
    size: (i32, i32),
    buffer: WlBuffer,
}

pub struct ExtCaptureState {
    pending: Vec<Pending>,
    clock: Clock<Monotonic>,
}

impl ExtCaptureState {
    /// Registers the globals (`window` = also offer window capture; the toplevel list is created by the caller).
    pub fn new(dh: &DisplayHandle, window: bool) -> Self {
        dh.create_global::<Vesper, ExtOutputImageCaptureSourceManagerV1, ()>(1, ());
        if window {
            dh.create_global::<Vesper, ExtForeignToplevelImageCaptureSourceManagerV1, ()>(1, ());
        }
        dh.create_global::<Vesper, ExtImageCopyCaptureManagerV1, ()>(1, ());
        Self { pending: Vec::new(), clock: Clock::new() }
    }

    /// Is a client waiting for a frame?
    pub fn wants(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Fill the waiting frames: monitors from the picture of `output`, windows from a render of the window alone.
    #[allow(clippy::too_many_arguments)]
    pub fn fulfil(&mut self, renderer: &mut GlesRenderer, output: &Output, space: &Space<Window>, size: Size<i32, Physical>, scale: f64, with_cursor: &[Element], clean: &[Element]) {
        let now: std::time::Duration = self.clock.now().into();
        let mut shots: [Option<Vec<u8>>; 2] = [None, None];
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            if !p.frame.is_alive() {
                continue;
            }
            let (rgba, w, flip) = match &p.source {
                Source::Output(o) if o == output => {
                    let slot = usize::from(p.cursor);
                    if shots[slot].is_none() {
                        shots[slot] = crate::screenshot::grab(renderer, if p.cursor { with_cursor } else { clean }, size, scale);
                    }
                    (shots[slot].clone(), size.w as usize, output.current_transform() == smithay::utils::Transform::Flipped180)
                }
                Source::Output(_) => {
                    self.pending.push(p); // another monitor: its own render handles it
                    continue;
                }
                Source::Window(handle) => {
                    let Some(window) = window_of(space, handle) else {
                        fail(&p, FailureReason::Unknown);
                        continue;
                    };
                    let geo = window.geometry();
                    let (w, h) = ((geo.size.w as f64 * scale).round() as i32, (geo.size.h as f64 * scale).round() as i32);
                    if (w, h) != p.size {
                        fail(&p, FailureReason::BufferConstraints);
                        announce_size(&p.session, (w, h));
                        continue;
                    }
                    let els: Vec<Element> = window.render_elements(renderer, Point::<i32, Physical>::from((0, 0)), Scale::from(scale), 1.0);
                    (crate::screenshot::grab(renderer, &els, (w, h).into(), scale), w as usize, false)
                }
            };
            let Some(rgba) = rgba else {
                fail(&p, FailureReason::Unknown);
                continue;
            };
            if write_buffer(&p.buffer, &rgba, w, p.size, flip) {
                let secs = now.as_secs();
                p.frame.transform(wl_output::Transform::Normal);
                p.frame.damage(0, 0, p.size.0, p.size.1);
                p.frame.presentation_time((secs >> 32) as u32, secs as u32, now.subsec_nanos());
                p.frame.ready();
                finish(&p.session);
            } else {
                fail(&p, FailureReason::BufferConstraints);
            }
        }
    }

    fn drop_frame(&mut self, frame: &ExtImageCopyCaptureFrameV1) {
        self.pending.retain(|p| &p.frame != frame);
    }
}

fn window_of(space: &Space<Window>, handle: &ForeignToplevelHandle) -> Option<Window> {
    let id = handle.identifier();
    space.elements().find(|w| w.user_data().get::<WindowHandle>().is_some_and(|h| h.0.identifier() == id)).cloned()
}

fn fail(p: &Pending, reason: FailureReason) {
    p.frame.failed(reason);
    finish(&p.session);
}

fn finish(session: &ExtImageCopyCaptureSessionV1) {
    if let Some(d) = session.data::<SessionData>() {
        *d.busy.lock().unwrap() = false;
    }
}

fn announce_size(session: &ExtImageCopyCaptureSessionV1, (w, h): (i32, i32)) {
    if let Some(d) = session.data::<SessionData>() {
        *d.size.lock().unwrap() = (w, h);
    }
    session.buffer_size(w as u32, h as u32);
    session.shm_format(FORMAT);
    session.done();
}

/// The size a source is captured at, in pixels.
fn source_size(state: &Vesper, source: &Source) -> Option<(i32, i32)> {
    match source {
        Source::Output(o) => o.current_mode().map(|m| (m.size.w, m.size.h)),
        Source::Window(handle) => {
            let scale = state.space.outputs().next().map_or(1.0, |o| o.current_scale().fractional_scale());
            let w = window_of(&state.space, handle)?;
            let g = w.geometry();
            Some(((g.size.w as f64 * scale).round() as i32, (g.size.h as f64 * scale).round() as i32)).filter(|(w, h)| *w > 0 && *h > 0)
        }
    }
}

/// Copy the top-down RGBA picture (`full_w` wide) into the client's buffer as XRGB.
fn write_buffer(buffer: &WlBuffer, rgba: &[u8], full_w: usize, (w, h): (i32, i32), flip: bool) -> bool {
    let (rw, rh) = (w as usize, h as usize);
    if full_w < rw || rgba.len() < rw * rh * 4 {
        return false;
    }
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        let (stride, offset) = (data.stride as usize, data.offset as usize);
        if data.width as usize != rw || data.height as usize != rh || offset + stride * rh > len {
            return false;
        }
        for y in 0..rh {
            let sy = if flip { rh - 1 - y } else { y };
            let src = &rgba[sy * full_w * 4..][..rw * 4];
            // SAFETY: bounds checked above; the client may write too, which at worst garbles this one frame.
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset + y * stride), rw * 4) };
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
            }
        }
        true
    })
    .unwrap_or(false)
}

// ---- the capture source managers ----

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Vesper {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<ExtOutputImageCaptureSourceManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(r, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Vesper {
    fn request(_: &mut Self, _: &Client, _: &ExtOutputImageCaptureSourceManagerV1, request: ext_output_image_capture_source_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource { source, output } = request {
            match Output::from_resource(&output) {
                Some(o) => {
                    init.init(source, Source::Output(o));
                }
                None => {
                    // an output that is gone: a source that can never produce a frame
                    init.init(source, Source::Output(Output::new("gone".into(), smithay::output::PhysicalProperties { size: (0, 0).into(), subpixel: smithay::output::Subpixel::Unknown, make: String::new(), model: String::new() })));
                }
            }
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Vesper {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<ExtForeignToplevelImageCaptureSourceManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(r, ());
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Vesper {
    fn request(_: &mut Self, _: &Client, _: &ExtForeignToplevelImageCaptureSourceManagerV1, request: ext_foreign_toplevel_image_capture_source_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource { source, toplevel_handle } = request {
            let handle: &ExtForeignToplevelHandleV1 = &toplevel_handle;
            match ForeignToplevelHandle::from_resource(handle) {
                Some(h) => {
                    init.init(source, Source::Window(h));
                }
                None => {
                    init.init(source, Source::Output(Output::new("gone".into(), smithay::output::PhysicalProperties { size: (0, 0).into(), subpixel: smithay::output::Subpixel::Unknown, make: String::new(), model: String::new() })));
                }
            }
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for Vesper {
    fn request(_: &mut Self, _: &Client, _: &ExtImageCaptureSourceV1, _: smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_image_capture_source_v1::Request, _: &Source, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

// ---- the copy-capture manager, sessions and frames ----

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Vesper {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<ExtImageCopyCaptureManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(r, ());
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Vesper {
    fn request(state: &mut Self, _: &Client, _: &ExtImageCopyCaptureManagerV1, request: ext_image_copy_capture_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession { session, source, options } => {
                let Some(src) = source.data::<Source>().cloned() else { return };
                let cursor = matches!(options, WEnum::Value(o) if o.contains(Options::PaintCursors));
                let size = source_size(state, &src);
                let s = init.init(session, SessionData { source: src, cursor, size: Mutex::new(size.unwrap_or((0, 0))), busy: Mutex::new(false) });
                match size {
                    Some((w, h)) => {
                        s.buffer_size(w as u32, h as u32);
                        s.shm_format(FORMAT);
                        s.done();
                    }
                    None => s.stopped(),
                }
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession { session, .. } => {
                // the cursor is painted into the picture (or left out); no separate cursor stream
                init.init(session, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for Vesper {
    fn request(_: &mut Self, _: &Client, _: &ExtImageCopyCaptureCursorSessionV1, request: ext_image_copy_capture_cursor_session_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } = request {
            let s = init.init(session, SessionData { source: Source::Output(Output::new("none".into(), smithay::output::PhysicalProperties { size: (0, 0).into(), subpixel: smithay::output::Subpixel::Unknown, make: String::new(), model: String::new() })), cursor: false, size: Mutex::new((0, 0)), busy: Mutex::new(false) });
            s.stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for Vesper {
    fn request(state: &mut Self, _: &Client, session: &ExtImageCopyCaptureSessionV1, request: ext_image_copy_capture_session_v1::Request, data: &SessionData, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            {
                let mut busy = data.busy.lock().unwrap();
                if *busy {
                    session.post_error(ext_image_copy_capture_session_v1::Error::DuplicateFrame, "a frame of this session is still in flight");
                    return;
                }
                *busy = true;
            }
            // a window may have been resized since the size was announced
            let size = *data.size.lock().unwrap();
            let f = init.init(frame, FrameData { session: session.clone(), source: data.source.clone(), cursor: data.cursor, size, buffer: Mutex::new(None), captured: Mutex::new(false) });
            if let Some(now) = source_size(state, &data.source) {
                if now != size {
                    announce_size(session, now);
                    f.failed(FailureReason::BufferConstraints);
                    *data.busy.lock().unwrap() = false;
                }
            } else {
                f.failed(FailureReason::Stopped);
                session.stopped();
            }
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for Vesper {
    fn request(state: &mut Self, _: &Client, frame: &ExtImageCopyCaptureFrameV1, request: ext_image_copy_capture_frame_v1::Request, data: &FrameData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                if *data.captured.lock().unwrap() {
                    frame.post_error(ext_image_copy_capture_frame_v1::Error::AlreadyCaptured, "the frame was already captured");
                    return;
                }
                *data.buffer.lock().unwrap() = Some(buffer);
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer { x, y, width, height } => {
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage, "damage must be positive");
                }
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                {
                    let mut captured = data.captured.lock().unwrap();
                    if *captured {
                        frame.post_error(ext_image_copy_capture_frame_v1::Error::AlreadyCaptured, "the frame was already captured");
                        return;
                    }
                    *captured = true;
                }
                let Some(buffer) = data.buffer.lock().unwrap().clone() else {
                    frame.post_error(ext_image_copy_capture_frame_v1::Error::NoBuffer, "capture without a buffer");
                    return;
                };
                let (w, h) = data.size;
                let ok = with_buffer_contents(&buffer, |_, len, b| b.format == FORMAT && b.width == w && b.height == h && b.stride >= w * 4 && b.offset >= 0 && b.offset as usize + b.stride as usize * h as usize <= len).unwrap_or(false);
                if !ok {
                    frame.failed(FailureReason::BufferConstraints);
                    finish(&data.session);
                    return;
                }
                if let Some(e) = state.extcapture.as_mut() {
                    e.pending.push(Pending { frame: frame.clone(), session: data.session.clone(), source: data.source.clone(), cursor: data.cursor, size: data.size, buffer });
                }
            }
            _ => {} // destroy
        }
    }

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, frame: &ExtImageCopyCaptureFrameV1, data: &FrameData) {
        if let Some(e) = state.extcapture.as_mut() {
            e.drop_frame(frame);
        }
        finish(&data.session);
    }
}

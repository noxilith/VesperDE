//! `wlr-screencopy-unstable-v1` (version 3, shm buffers only).
//!
//! This is what lets `grim`, `wf-recorder` and `xdg-desktop-portal-wlr` capture a monitor, and through the portal
//! gives browsers, Discord, OBS and Zoom their screen sharing. A client asks for a frame, learns the buffer it must
//! provide (`buffer` + `buffer_done`), attaches an shm buffer with `copy`, and the compositor fills it on the next
//! render. The picture comes from the same offscreen render the screenshots use ([`crate::screenshot::grab`]).

use std::sync::Mutex;

use smithay::{
    backend::renderer::gles::GlesRenderer,
    output::Output,
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_shm},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Clock, Monotonic, Physical, Rectangle, Size},
    wayland::shm::{with_buffer_contents, with_buffer_contents_mut},
};

use crate::{render::Element, state::Vesper};

/// The only format offered: it is always in the shm global and every consumer (grim, xdpw, wf-recorder) takes it.
const FORMAT: wl_shm::Format = wl_shm::Format::Xrgb8888;

/// Compositor-side state: the global and the copies waiting for the next render.
pub struct ScreencopyState {
    pending: Vec<Pending>,
    clock: Clock<Monotonic>,
}

/// A frame whose client has attached a buffer.
pub struct Pending {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    output: Output,
    /// Area of the output to copy, in output pixels.
    rect: Rectangle<i32, Physical>,
    cursor: bool,
    damage: bool,
}

/// Per-frame data: what was asked for, checked when the buffer arrives.
pub struct FrameData {
    output: Output,
    rect: Rectangle<i32, Physical>,
    cursor: bool,
    used: Mutex<bool>,
}

impl ScreencopyState {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<Vesper, ZwlrScreencopyManagerV1, ()>(3, ());
        Self { pending: Vec::new(), clock: Clock::new() }
    }

    /// Is a client waiting for a copy of `output`?
    pub fn wants(&self, output: &Output) -> bool {
        self.pending.iter().any(|p| &p.output == output)
    }

    /// Take the waiting copies of `output` (the ones whose client is still there).
    pub fn take(&mut self, output: &Output) -> Vec<Pending> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|p| &p.output == output);
        self.pending = rest;
        mine.into_iter().filter(|p| p.frame.is_alive()).collect()
    }

    /// Fill the buffers of `frames` from the picture of the whole output. `with_cursor` is the picture including the
    /// pointer, `clean` the one without; frames that did not ask for the cursor get `clean`.
    pub fn fulfil(&mut self, frames: Vec<Pending>, renderer: &mut GlesRenderer, size: Size<i32, Physical>, scale: f64, with_cursor: &[Element], clean: &[Element]) {
        if frames.is_empty() {
            return;
        }
        let mut shots: [Option<Vec<u8>>; 2] = [None, None];
        let now: std::time::Duration = self.clock.now().into();
        for p in frames {
            let slot = usize::from(p.cursor);
            if shots[slot].is_none() {
                shots[slot] = crate::screenshot::grab(renderer, if p.cursor { with_cursor } else { clean }, size, scale);
            }
            let Some(rgba) = &shots[slot] else {
                p.frame.failed();
                continue;
            };
            // the nested (winit) output says Flipped180 and clients such as grim undo that, so hand them the flipped rows
            let flip = p.output.current_transform() == smithay::utils::Transform::Flipped180;
            if write_buffer(&p, rgba, size.w as usize, flip) {
                p.frame.flags(Flags::empty());
                if p.damage {
                    p.frame.damage(0, 0, p.rect.size.w as u32, p.rect.size.h as u32);
                }
                let secs = now.as_secs();
                p.frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
            } else {
                p.frame.failed();
            }
        }
    }
}

/// Copy `rect` of the top-down RGBA picture (`full_w` pixels wide) into the client's buffer as XRGB.
fn write_buffer(p: &Pending, rgba: &[u8], full_w: usize, flip: bool) -> bool {
    let (rx, ry, rw, rh) = (p.rect.loc.x as usize, p.rect.loc.y as usize, p.rect.size.w as usize, p.rect.size.h as usize);
    with_buffer_contents_mut(&p.buffer, |ptr, len, data| {
        let (stride, offset) = (data.stride as usize, data.offset as usize);
        if data.width as usize != rw || data.height as usize != rh || offset + stride * rh > len {
            return false;
        }
        for y in 0..rh {
            let sy = if flip { rh - 1 - y } else { y };
            let src = &rgba[((ry + sy) * full_w + rx) * 4..][..rw * 4];
            // SAFETY: the bounds were checked above; the client may write to the memory too, which at worst garbles
            // this one frame, so we only ever write through a raw slice and never read it back.
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset + y * stride), rw * 4) };
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                // RGBA in memory -> BGRX (the little-endian XRGB8888 word)
                d.copy_from_slice(&[s[2], s[1], s[0], 0xff]);
            }
        }
        true
    })
    .unwrap_or(false)
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Vesper {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrScreencopyManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Vesper {
    fn request(_: &mut Self, _: &Client, _: &ZwlrScreencopyManagerV1, request: zwlr_screencopy_manager_v1::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        let (frame, overlay_cursor, output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput { frame, overlay_cursor, output } => (frame, overlay_cursor, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion { frame, overlay_cursor, output, x, y, width, height } => {
                (frame, overlay_cursor, output, Some((x, y, width, height)))
            }
            _ => return,
        };
        let cursor = overlay_cursor != 0;
        let Some(out) = Output::from_resource(&output) else {
            let frame = data_init.init(frame, FrameData { output: dummy_output(), rect: Rectangle::default(), cursor, used: Mutex::new(true) });
            frame.failed();
            return;
        };
        let Some(mode) = out.current_mode() else {
            let frame = data_init.init(frame, FrameData { output: out, rect: Rectangle::default(), cursor, used: Mutex::new(true) });
            frame.failed();
            return;
        };
        let full = Rectangle::<i32, Physical>::from_size(mode.size);
        let scale = out.current_scale().fractional_scale();
        // a region is in logical pixels of the output; the buffer is in output pixels
        let rect = match region {
            None => Some(full),
            Some((x, y, w, h)) if w > 0 && h > 0 => {
                let r = Rectangle::<i32, Physical>::new(
                    ((x as f64 * scale).round() as i32, (y as f64 * scale).round() as i32).into(),
                    (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1)).into(),
                );
                r.intersection(full)
            }
            _ => None,
        };
        let Some(rect) = rect.filter(|r| r.size.w > 0 && r.size.h > 0) else {
            let frame = data_init.init(frame, FrameData { output: out, rect: Rectangle::default(), cursor, used: Mutex::new(true) });
            frame.failed();
            return;
        };
        let frame = data_init.init(frame, FrameData { output: out, rect, cursor, used: Mutex::new(false) });
        frame.buffer(FORMAT, rect.size.w as u32, rect.size.h as u32, rect.size.w as u32 * 4);
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for Vesper {
    fn request(state: &mut Self, _: &Client, frame: &ZwlrScreencopyFrameV1, request: zwlr_screencopy_frame_v1::Request, data: &FrameData, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let (buffer, damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return, // destroy
        };
        {
            let mut used = data.used.lock().unwrap();
            if *used {
                // a frame that already failed or was copied: copying again is the client's mistake
                frame.post_error(zwlr_screencopy_frame_v1::Error::AlreadyUsed, "the frame was already copied");
                return;
            }
            *used = true;
        }
        let (w, h) = (data.rect.size.w, data.rect.size.h);
        let ok = with_buffer_contents(&buffer, |_, len, b| {
            b.format == FORMAT && b.width == w && b.height == h && b.stride >= w * 4 && b.offset >= 0 && b.offset as usize + b.stride as usize * h as usize <= len
        })
        .unwrap_or(false);
        if !ok {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, "the buffer does not match the frame (shm, XRGB8888, same size)");
            return;
        }
        state.screencopy.pending.push(Pending { frame: frame.clone(), buffer, output: data.output.clone(), rect: data.rect, cursor: data.cursor, damage });
    }

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, frame: &ZwlrScreencopyFrameV1, _: &FrameData) {
        state.screencopy.pending.retain(|p| &p.frame != frame);
    }
}

/// An output that is never advertised, only there so a failed frame has something to hold.
fn dummy_output() -> Output {
    Output::new("none".into(), smithay::output::PhysicalProperties { size: (0, 0).into(), subpixel: smithay::output::Subpixel::Unknown, make: String::new(), model: String::new() })
}

//! Real-hardware backend: libseat session, DRM/KMS output, libinput.
//!
//! Single GPU (the primary one), one `DrmCompositor` per connected connector.
//! Multi-GPU, DRM leasing and dmabuf clients are future work.

use std::{collections::HashMap, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay::reexports::drm::{self, control::Device as ControlDevice};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
        },
        egl::{context::ContextPriority, EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        input::InputEvent,
        renderer::{gles::GlesRenderer, ImportMemWl},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::primary_gpu,
    },
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{timer::{TimeoutAction, Timer}, EventLoop},
        drm::control::{connector, crtc, ModeTypeFlags},
        input::{AccelProfile, ClickMethod, Device, DeviceCapability, Libinput},
        rustix::fs::OFlags,
    },
    utils::DeviceFd,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use smithay::desktop::layer_map_for_output;
use crate::state::Vesper;

const COLOR_FORMATS: [Fourcc; 4] = [Fourcc::Abgr2101010, Fourcc::Argb2101010, Fourcc::Abgr8888, Fourcc::Argb8888];
const CLEAR: [f32; 4] = [0.08, 0.08, 0.1, 1.0];

type Compositor = DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

struct Surface {
    output: Output,
    compositor: Compositor,
    connector: connector::Handle,
    modes: Vec<drm::control::Mode>,
    phys_mm: (i32, i32),
    off: bool,
    /// A frame was queued and its vblank has not arrived yet.
    pending: bool,
}

pub struct Tty {
    session: LibSeatSession,
    renderer: GlesRenderer,
    drm: DrmDevice,
    gbm: GbmDevice<DrmDeviceFd>,
    allocator: GbmAllocator<DrmDeviceFd>,
    exporter: GbmFramebufferExporter<DrmDeviceFd>,
    scanner: DrmScanner,
    surfaces: HashMap<crtc::Handle, Surface>,
    libinput: Libinput,
    cursor: crate::cursor::CursorSet,
    devices: Vec<Device>,
}

pub fn init(event_loop: &EventLoop<'static, Vesper>, state: &mut Vesper) -> Result<()> {
    let (session, notifier) = LibSeatSession::new()
        .map_err(|e| anyhow!("could not open a seat session ({e}); run from a real VT (not over ssh) with logind or seatd available"))?;
    let seat = session.seat();

    // --- GPU
    let path = primary_gpu(&seat)
        .context("looking for the primary GPU")?
        .ok_or_else(|| anyhow!("no GPU found on {seat}"))?;
    tracing::info!("using GPU {}", path.display());
    let fd = session
        .clone()
        .open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
        .map_err(|e| anyhow!("opening {}: {e:?}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true).context("creating DRM device")?;
    let gbm = GbmDevice::new(fd).context("creating GBM device")?;
    let egl = unsafe { EGLDisplay::new(gbm.clone()) }.context("creating EGL display")?;
    let context = EGLContext::new_with_priority(&egl, ContextPriority::High).context("creating EGL context")?;
    let renderer = unsafe { GlesRenderer::new(context) }.context("creating GLES renderer")?;

    let render_node = DrmNode::from_path(&path).ok().and_then(|n| n.node_with_type(NodeType::Render)?.ok());
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), render_node.into());

    // --- input
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat).map_err(|_| anyhow!("libinput could not take seat {seat}"))?;
    let libinput_backend = LibinputInputBackend::new(libinput.clone());

    let handle = event_loop.handle();
    handle
        .insert_source(libinput_backend, |mut event, _, state| {
            match &mut event {
                InputEvent::DeviceAdded { device } => {
                    configure_device(&state.config.input, device);
                    if let Some(t) = state.tty.as_mut() {
                        t.devices.push(device.clone());
                    }
                }
                InputEvent::DeviceRemoved { device } => {
                    if let Some(t) = state.tty.as_mut() {
                        t.devices.retain(|d| d != device);
                    }
                }
                _ => {}
            }
            state.process_input_event(event)
        })
        .map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(drm_notifier, |event, _, state| match event {
            DrmEvent::VBlank(crtc) => state.vblank(crtc),
            DrmEvent::Error(err) => tracing::error!("drm error: {err:?}"),
        })
        .map_err(|e| anyhow!("{e}"))?;

    handle
        .insert_source(notifier, |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused (switched away)");
                state.ptt_release(); // a key held while switching away never reports its release
                if let Some(tty) = state.tty.as_mut() {
                    tty.libinput.suspend();
                    tty.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                state.night_applied = None; // the colour ramps are reset when another VT was in front
                if let Some(tty) = state.tty.as_mut() {
                    if let Err(err) = tty.libinput.resume() {
                        tracing::error!("libinput resume failed: {err:?}");
                    }
                    if let Err(err) = tty.drm.activate(false) {
                        tracing::error!("drm activate failed: {err}");
                    }
                    for s in tty.surfaces.values_mut() {
                        let _ = s.compositor.reset_state();
                        s.pending = false;
                    }
                    let crtcs: Vec<_> = tty.surfaces.keys().copied().collect();
                    for crtc in crtcs {
                        state.loop_handle.insert_idle(move |s| s.render(crtc));
                    }
                }
            }
        })
        .map_err(|e| anyhow!("{e}"))?;

    state.shm_state.update_formats(renderer.shm_formats());
    crate::dmabuf::create_global(state, &renderer);

    let vt_session = session.clone();
    state.vt_switch = Some(Box::new(move |vt| {
        let mut s = vt_session.clone();
        if let Err(err) = s.change_vt(vt) {
            tracing::error!("VT switch failed: {err:?}");
        }
    }));

    state.tty = Some(Tty {
        session,
        renderer,
        drm,
        gbm,
        allocator,
        exporter,
        scanner: DrmScanner::new(),
        surfaces: HashMap::new(),
        libinput,
        cursor: crate::cursor::CursorSet::load(&state.config.appearance.cursor_theme, state.config.appearance.cursor_size, 1),
        devices: Vec::new(),
    });
    state.scan_connectors();
    if state.output().is_none() {
        return Err(anyhow!("no connected display found"));
    }
    Ok(())
}

impl Vesper {
    fn scan_connectors(&mut self) {
        let Some(tty) = self.tty.as_mut() else { return };
        let events: Vec<_> = match tty.scanner.scan_connectors(&tty.drm) {
            Ok(result) => result.into_iter().collect(),
            Err(err) => {
                tracing::error!("connector scan failed: {err}");
                return;
            }
        };
        for event in events {
            match event {
                DrmScanEvent::Connected { connector, crtc: Some(crtc) } => self.connector_connected(connector, crtc),
                DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => {
                    if let Some(s) = self.tty.as_mut().and_then(|t| t.surfaces.remove(&crtc)) {
                        self.space.unmap_output(&s.output);
                        self.apply_output_config();
                    }
                }
                _ => {}
            }
        }
    }

    fn connector_connected(&mut self, connector: connector::Info, crtc: crtc::Handle) {
        let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
        let modes: Vec<drm::control::Mode> = connector.modes().to_vec();
        let Some(mode) = self.chosen_mode(&name, &modes) else {
            tracing::warn!("{name}: no modes");
            return;
        };
        let wl_mode = WlMode::from(mode);
        let (pw, ph) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (pw as i32, ph as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: "Unknown".into(),
            },
        );
        output.create_global::<Vesper>(&self.display_handle);
        for m in &modes {
            output.add_mode(WlMode::from(*m));
        }
        output.set_preferred(WlMode::from(modes.iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).copied().unwrap_or(mode)));
        let scale = self.chosen_scale(&name, wl_mode.size.w, pw as i32);
        output.change_current_state(Some(wl_mode), None, Some(smithay::output::Scale::Fractional(scale)), Some((0, 0).into()));
        self.space.map_output(&output, (0, 0));

        let tty = self.tty.as_mut().unwrap();
        let surface = match tty.drm.create_surface(crtc, mode, &[connector.handle()]) {
            Ok(s) => s,
            Err(err) => {
                tracing::error!("{name}: create_surface: {err}");
                return;
            }
        };
        let compositor = match DrmCompositor::new(
            &output,
            surface,
            None,
            tty.allocator.clone(),
            tty.exporter.clone(),
            COLOR_FORMATS,
            tty.renderer.egl_context().dmabuf_render_formats().clone(),
            tty.drm.cursor_size(),
            Some(tty.gbm.clone()),
        ) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!("{name}: compositor: {err}");
                return;
            }
        };
        tracing::info!("{name}: {}x{}@{:.2}Hz", wl_mode.size.w, wl_mode.size.h, wl_mode.refresh as f64 / 1000.0);
        tty.surfaces.insert(crtc, Surface { output, compositor, connector: connector.handle(), modes, phys_mm: (pw as i32, ph as i32), off: false, pending: false });
        self.apply_output_config();
        self.loop_handle.insert_idle(move |s| s.render(crtc));
    }

    fn vblank(&mut self, crtc: crtc::Handle) {
        if let Some(s) = self.tty.as_mut().and_then(|t| t.surfaces.get_mut(&crtc)) {
            if let Err(err) = s.compositor.frame_submitted() {
                tracing::warn!("frame_submitted: {err}");
            }
            s.pending = false;
        }
        self.render(crtc);
    }

    fn schedule_render(&self, crtc: crtc::Handle) {
        let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(16)), move |_, _, s| {
            s.render(crtc);
            TimeoutAction::Drop
        });
    }

    pub fn render(&mut self, crtc: crtc::Handle) {
        let Some(output) = self.tty.as_ref().and_then(|t| t.surfaces.get(&crtc)).map(|s| s.output.clone()) else { return };
        let lock_surfaces = self.lock_surfaces_for(&output);
        let pointer_loc = if lock_surfaces.is_some() { None } else { self.seat.get_pointer().map(|p| p.current_location()) };
        let capture = self.pending_capture.take();
        let crop = capture.and_then(|k| self.capture_crop(k));
        let freeze = match capture {
            Some(crate::screenshot::CaptureKind::Freeze(p)) => Some((self.freeze_windows(), p)),
            _ => None,
        };
        let shot_tx = self.shot_tx.clone();
        let shot_dir = self.config.screenshots.dir.clone();
        if !self.round_tried {
            self.round_tried = true;
            if let Some(t) = self.tty.as_mut() {
                self.round = crate::round::RoundShaders::compile(&mut t.renderer);
            }
        }
        // night light: colour ramps on the monitors; if they refuse, a tint over the picture does the job
        let wanted = self.config.appearance.night_light.then_some(self.config.appearance.night_light_strength);
        if self.night_applied != Some(wanted) {
            self.night_via_gamma = self.apply_gamma(wanted);
            self.night_applied = Some(wanted);
        }
        self.animate();
        let borders = self.border_elements(&output);
        let blur_params = self.blur_params(&output);
        let hint = self.hint_element(&output);
        let corner = self.config.appearance.corner_radius.clamp(0, 64);
        let mut flags = FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT | FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT;
        if self.config.display.overlay_planes {
            flags |= FrameFlags::ALLOW_OVERLAY_PLANE_SCANOUT;
        }
        let (cursor_theme, cursor_size, cursor_scale) = (self.config.appearance.cursor_theme.clone(), self.config.appearance.cursor_size, output.current_scale().integer_scale().max(1) as u32);
        let Some(tty) = self.tty.as_mut() else { return };
        if !tty.cursor.is_for(&cursor_theme, cursor_size, cursor_scale) {
            tty.cursor = crate::cursor::CursorSet::reload(&tty.cursor, &cursor_theme, cursor_size, cursor_scale);
        }
        if !tty.session.is_active() {
            return;
        }
        let Some(surface) = tty.surfaces.get_mut(&crtc) else { return };
        if self.screens_off || surface.off {
            self.pending_capture = capture;
            return; // nothing to draw on a monitor that is off
        }
        if surface.pending {
            self.pending_capture = capture; // the screenshot waits for the next frame
            return; // a frame is already on its way; the vblank renders the next one
        }
        let renderer = &mut tty.renderer;

        let elements = crate::render::output_elements(
            renderer,
            &self.space,
            &output,
            pointer_loc.map(|l| (l, &self.cursor_status, &tty.cursor)),
            borders.clone(),
            hint.clone(),
            Some((&mut self.blur, &blur_params)),
            self.round.as_ref().map(|r| (r, corner)),
            lock_surfaces.as_deref(),
        );

        // screen capture clients (wlr-screencopy: grim, the screen-sharing portal) get this frame
        if self.extcapture.as_ref().is_some_and(|e| e.wants()) {
            if let Some(mode) = output.current_mode() {
                let clean = crate::render::output_elements(renderer, &self.space, &output, None, borders.clone(), hint.clone(), Some((&mut self.blur, &blur_params)), self.round.as_ref().map(|r| (r, corner)), lock_surfaces.as_deref());
                if let Some(e) = self.extcapture.as_mut() {
                    e.fulfil(renderer, &output, &self.space, mode.size, output.current_scale().fractional_scale(), &elements, &clean);
                }
            }
        }
        if self.screencopy.wants(&output) {
            if let Some(mode) = output.current_mode() {
                let frames = self.screencopy.take(&output);
                let clean = crate::render::output_elements(renderer, &self.space, &output, None, borders.clone(), hint.clone(), Some((&mut self.blur, &blur_params)), self.round.as_ref().map(|r| (r, corner)), lock_surfaces.as_deref());
                self.screencopy.fulfil(frames, renderer, mode.size, output.current_scale().fractional_scale(), &elements, &clean);
            }
        }

        if capture.is_some() {
            if let Some(mode) = output.current_mode() {
                // the screenshot never contains the pointer
                let clean = crate::render::output_elements(renderer, &self.space, &output, None, borders.clone(),
            hint.clone(), Some((&mut self.blur, &blur_params)), self.round.as_ref().map(|r| (r, corner)), lock_surfaces.as_deref());
                if let Some(rgba) = crate::screenshot::grab(renderer, &clean, mode.size, output.current_scale().fractional_scale()) {
                    if let Some((windows, pick)) = freeze {
                        crate::screenshot::write_freeze(rgba, mode.size.w as u32, mode.size.h as u32, windows, pick);
                    } else {
                        crate::screenshot::finish(rgba, mode.size.w as u32, mode.size.h as u32, crop, shot_dir, shot_tx);
                    }
                }
            }
        }

        let n_elements = elements.len();
        let mut render_failed = false;
        let queued = match surface.compositor.render_frame(renderer, &elements, CLEAR, flags) {
            Ok(res) if !res.is_empty => match surface.compositor.queue_frame(()) {
                Ok(()) => {
                    surface.pending = true;
                    true
                }
                Err(err) => {
                    tracing::warn!("queue_frame: {err}");
                    render_failed = true;
                    false
                }
            },
            Ok(_) => false,
            Err(err) => {
                tracing::warn!("render_frame: {err}");
                render_failed = true;
                false
            }
        };

        {
            let st = self.render_stats.entry(output.name()).or_default();
            match (queued, render_failed) {
                (true, _) => st.queued += 1,
                (false, true) => st.errors += 1,
                (false, false) => st.empty += 1,
            }
            st.elements = n_elements;
        }
        if self.stats_logged.elapsed() >= Duration::from_secs(5) {
            self.stats_logged = std::time::Instant::now();
            let stats = std::mem::take(&mut self.render_stats);
            if stats.values().any(|s| s.queued + s.errors > 0) {
                let mut names: Vec<_> = stats.keys().cloned().collect();
                names.sort();
                let line: Vec<String> = names.iter().map(|n| { let s = stats[n]; format!("{n}: {} drawn, {} unchanged, {} errors, {} elements", s.queued, s.empty, s.errors, s.elements) }).collect();
                tracing::info!("frames in the last 5 s: {}", line.join("; "));
            }
        }

        // Tell clients a frame happened so they can draw the next one. This must happen after every attempt, shown or not:
        // a client that asked for a callback and has nothing new on screen (Firefox while scrolling) would otherwise wait
        // for it and stall until something else woke us up.
        // When nothing was queued this is throttled to one pass per 16 ms: a client that commits without damage and asks
        // for a new callback each time would otherwise wake the loop again at once, and the loop would spin at 100% CPU.
        if queued {
            self.send_frames(&output, lock_surfaces.as_deref());
        } else if !self.frame_timer {
            self.frame_timer = true;
            let _ = self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(16)), move |_, _, s| {
                s.frame_timer = false;
                if let Some(output) = s.tty.as_ref().and_then(|t| t.surfaces.get(&crtc)).map(|x| x.output.clone()) {
                    let locks = s.lock_surfaces_for(&output);
                    s.send_frames(&output, locks.as_deref());
                }
                TimeoutAction::Drop
            });
        }
        self.popups.cleanup();
        let _ = self.display_handle.flush_clients();

        // Nothing was drawn, so no vblank will come. We render again on the next event (see `redraw_all`); only a running
        // animation needs a timer to keep going.
        if !queued && self.animating() {
            self.schedule_render(crtc);
        }
    }
}

impl Vesper {
    fn send_frames(&mut self, output: &smithay::output::Output, locks: Option<&[smithay::reexports::wayland_server::protocol::wl_surface::WlSurface]>) {
        let now = self.start_time.elapsed();
        for w in self.space.elements() {
            w.send_frame(output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        for s in locks.into_iter().flatten() {
            smithay::desktop::utils::send_frames_surface_tree(s, output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        let _ = self.display_handle.flush_clients();
    }
}

impl Tty {
    pub fn import_dmabuf(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        use smithay::backend::renderer::ImportDma;
        self.renderer.import_dmabuf(dmabuf, None).is_ok()
    }
}

/// Apply the configured touchpad / mouse settings to a newly appeared device.
fn configure_device(cfg: &vesper_config::Input, device: &mut Device) {
    // Errors just mean the device doesn't support that setting, which is fine.
    if device.config_tap_finger_count() > 0 {
        let t = &cfg.touchpad;
        let _ = device.config_tap_set_enabled(t.tap);
        if device.config_scroll_has_natural_scroll() {
            let _ = device.config_scroll_set_natural_scroll_enabled(t.natural_scroll);
        }
        if device.config_accel_is_available() {
            let _ = device.config_accel_set_speed(t.accel_speed.clamp(-1.0, 1.0));
        }
        if device.config_dwt_is_available() {
            let _ = device.config_dwt_set_enabled(t.disable_while_typing);
        }
        match t.click_method.as_str() {
            "clickfinger" => {
                let _ = device.config_click_set_method(ClickMethod::Clickfinger);
            }
            "buttonareas" => {
                let _ = device.config_click_set_method(ClickMethod::ButtonAreas);
            }
            _ => {}
        }
        let _ = device.config_middle_emulation_set_enabled(t.middle_emulation);
        tracing::info!("configured touchpad {:?}", device.name());
    } else if device.has_capability(DeviceCapability::Pointer) {
        let m = &cfg.mouse;
        if device.config_scroll_has_natural_scroll() {
            let _ = device.config_scroll_set_natural_scroll_enabled(m.natural_scroll);
        }
        if device.config_accel_is_available() {
            let _ = device.config_accel_set_speed(m.accel_speed.clamp(-1.0, 1.0));
            let profile = if m.accel_profile == "flat" { AccelProfile::Flat } else { AccelProfile::Adaptive };
            let _ = device.config_accel_set_profile(profile);
        }
    }
}

// ---------------------------------------------------------------- monitor settings, power, input reconfiguration

impl Vesper {
    /// The mode for a connector: `[display.outputs.<name>] mode` if it matches, else the preferred one.
    fn chosen_mode(&self, name: &str, modes: &[drm::control::Mode]) -> Option<drm::control::Mode> {
        let list: Vec<(u32, u32, f64)> =
            modes.iter().map(|m| (m.size().0 as u32, m.size().1 as u32, m.vrefresh() as f64)).collect();
        let want = self.config.display.outputs.get(name).map(|o| o.mode.as_str()).unwrap_or("");
        crate::outputs::pick_mode(want, &list)
            .map(|i| modes[i])
            .or_else(|| modes.iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).copied())
            .or_else(|| modes.first().copied())
    }

    fn chosen_scale(&self, name: &str, px_w: i32, mm_w: i32) -> f64 {
        let s = self.config.display.outputs.get(name).map(|o| o.scale).unwrap_or(0.0);
        crate::outputs::clean_scale(if s > 0.0 { s } else { crate::outputs::auto_scale(px_w, mm_w) })
    }

    /// Bring every connected monitor in line with `[display]`: mode, scale, on/off and position.
    pub fn apply_output_config(&mut self) {
        let Some(tty) = self.tty.as_ref() else { return };
        let mut crtcs: Vec<_> = tty.surfaces.keys().copied().collect();
        crtcs.sort_by_key(|c| tty.surfaces[c].output.name());

        let mut placed: Vec<(crtc::Handle, Output, crate::outputs::Place)> = Vec::new();
        let mut hidden: Vec<Output> = Vec::new();
        for crtc in crtcs {
            let (name, modes, mm) = {
                let s = &self.tty.as_ref().unwrap().surfaces[&crtc];
                (s.output.name(), s.modes.clone(), s.phys_mm)
            };
            let cfg = self.config.display.outputs.get(&name).cloned().unwrap_or_default();
            let Some(mode) = self.chosen_mode(&name, &modes) else { continue };
            let wl_mode = WlMode::from(mode);
            let scale = self.chosen_scale(&name, wl_mode.size.w, mm.0);
            let tty = self.tty.as_mut().unwrap();
            let s = tty.surfaces.get_mut(&crtc).unwrap();
            if s.output.current_mode() != Some(wl_mode) {
                if let Err(err) = s.compositor.use_mode(mode) {
                    tracing::warn!("{name}: could not switch mode: {err}");
                }
            }
            s.output.change_current_state(Some(wl_mode), None, Some(smithay::output::Scale::Fractional(scale)), None);
            if cfg.enabled == Some(false) {
                s.off = true;
                hidden.push(s.output.clone());
                continue;
            }
            s.off = false;
            let logical_w = (wl_mode.size.w as f64 / scale).round() as i32;
            let logical_h = (wl_mode.size.h as f64 / scale).round() as i32;
            placed.push((
                crtc,
                s.output.clone(),
                crate::outputs::Place { w: logical_w, h: logical_h, at: cfg.x.zip(cfg.y).or_else(|| cfg.x.map(|x| (x, 0))).or_else(|| cfg.y.map(|y| (0, y))) },
            ));
        }
        for o in hidden {
            self.space.unmap_output(&o);
        }
        let positions = crate::outputs::arrange(&placed.iter().map(|(_, _, p)| crate::outputs::Place { w: p.w, h: p.h, at: p.at }).collect::<Vec<_>>());
        for ((crtc, output, _), pos) in placed.into_iter().zip(positions) {
            output.change_current_state(None, None, None, Some(pos.into()));
            self.space.map_output(&output, pos);
            tracing::info!("output {} at {},{} {}x{} scale {}", output.name(), pos.0, pos.1, output.current_mode().map_or(0, |m| m.size.w), output.current_mode().map_or(0, |m| m.size.h), output.current_scale().fractional_scale());
            self.loop_handle.insert_idle(move |s| s.render(crtc));
        }
        // a monitor was added, removed or changed: windows and bars need new sizes
        let outs: Vec<_> = self.space.outputs().cloned().collect();
        for o in outs {
            layer_map_for_output(&o).arrange();
        }
        self.dump_scene("outputs changed");
        self.send_preferred_scales();
        self.relayout();
        self.ipc_broadcast();
    }

    /// Re-apply touchpad and mouse settings to every device that is already plugged in.
    pub fn reconfigure_inputs(&mut self) {
        let cfg = self.config.input.clone();
        if let Some(tty) = self.tty.as_mut() {
            for d in tty.devices.iter_mut() {
                configure_device(&cfg, d);
            }
        }
    }

    /// Switch monitors off (DPMS) or back on. Returns whether anything changed.
    /// Set the night-light colour ramps on every monitor (None = neutral). Returns whether any monitor took them.
    pub fn apply_gamma(&mut self, strength: Option<u32>) -> bool {
        let Some(tty) = self.tty.as_mut() else { return false };
        let s = strength.map_or(0.0, |v| v.clamp(1, 100) as f32 / 100.0);
        let (green, blue) = (1.0 - 0.35 * s, 1.0 - 0.60 * s);
        let mut any = false;
        for crtc in tty.surfaces.keys() {
            let Ok(info) = tty.drm.get_crtc(*crtc) else { continue };
            let n = info.gamma_length() as usize;
            if n < 2 {
                continue;
            }
            let ramp = |scale: f32| -> Vec<u16> { (0..n).map(|i| ((i as f32 / (n - 1) as f32) * scale * 65535.0).round() as u16).collect() };
            match tty.drm.set_gamma(*crtc, &ramp(1.0), &ramp(green), &ramp(blue)) {
                Ok(()) => any = true,
                Err(err) => tracing::warn!("night light: could not set the colour ramps: {err}"),
            }
        }
        any
    }

    pub fn set_screens_off(&mut self, off: bool) -> bool {
        if self.screens_off == off {
            return false;
        }
        self.screens_off = off;
        let Some(tty) = self.tty.as_mut() else { return false };
        for s in tty.surfaces.values() {
            if let Ok(props) = tty.drm.get_properties(s.connector) {
                let (ids, _) = props.as_props_and_values();
                for id in ids {
                    if let Ok(info) = tty.drm.get_property(*id) {
                        if info.name().to_bytes() == b"DPMS" {
                            let value = if off { 3 } else { 0 };
                            if let Err(err) = tty.drm.set_property(s.connector, *id, value) {
                                tracing::warn!("{}: DPMS failed: {err}", s.output.name());
                            }
                        }
                    }
                }
            }
        }
        if !off {
            for s in tty.surfaces.values_mut() {
                s.pending = false;
            }
            let crtcs: Vec<_> = tty.surfaces.keys().copied().collect();
            for crtc in crtcs {
                self.loop_handle.insert_idle(move |s| s.render(crtc));
            }
        }
        true
    }
}

impl Vesper {
    /// Monitors switched off in the settings, so the settings window can list them and switch them back on.
    pub fn disabled_output_infos(&self) -> Vec<vesper_ipc::OutputInfo> {
        let Some(tty) = self.tty.as_ref() else { return Vec::new() };
        tty.surfaces
            .values()
            .filter(|s| s.off)
            .map(|s| {
                let fmt = |m: &drm::control::Mode| format!("{}x{}@{}", m.size().0, m.size().1, m.vrefresh());
                let mut modes: Vec<String> = s.modes.iter().map(fmt).collect();
                modes.dedup();
                vesper_ipc::OutputInfo { name: s.output.name(), modes, enabled: false, scale: 1.0, ..Default::default() }
            })
            .collect()
    }
}

impl Vesper {
    /// Try to draw every monitor. Called after each pass of the event loop: if nothing changed the renderer finds no
    /// damage and does nothing, and a monitor that still waits for its last frame is skipped, so this is cheap and
    /// means we only work when something happens (instead of polling 60 times a second).
    pub fn redraw_all(&mut self) {
        if let Some(hook) = &self.redraw_hook {
            if std::mem::take(&mut self.just_redrew) {
                return;
            }
            hook();
            return;
        }
        let Some(tty) = self.tty.as_ref() else { return };
        let crtcs: Vec<_> = tty.surfaces.iter().filter(|(_, s)| !s.pending && !s.off).map(|(c, _)| *c).collect();
        for crtc in crtcs {
            self.render(crtc);
        }
    }
}

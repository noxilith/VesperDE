//! Development backend: run Vesper as a window inside another compositor.

use std::{cell::RefCell, rc::Rc, time::Duration};

use anyhow::{anyhow, Result};
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::state::Vesper;

pub fn init(event_loop: &EventLoop<'static, Vesper>, state: &mut Vesper) -> Result<()> {
    let (backend, winit) = winit::init::<GlesRenderer>().map_err(|e| anyhow!("{e:?}"))?;
    let backend = Rc::new(RefCell::new(backend));

    let mode = Mode { size: backend.borrow().window_size(), refresh: 60_000 };
    let output = Output::new(
        "winit".into(),
        PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Vesper".into(), model: "winit".into() },
    );
    output.create_global::<Vesper>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.relayout();

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    crate::dmabuf::create_global(state, backend.borrow_mut().renderer());
    let importer = backend.clone();
    state.dmabuf_importer = Some(Box::new(move |d| {
        use smithay::backend::renderer::ImportDma;
        importer.borrow_mut().renderer().import_dmabuf(d, None).is_ok()
    }));

    let hook_backend = backend.clone();
    state.redraw_hook = Some(Box::new(move || {
        if let Ok(b) = hook_backend.try_borrow() {
            b.window().request_redraw();
        }
    }));

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
                state.relayout();
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                tracing::trace!(target: "vesper::wake", "winit redraw");
                if !state.round_tried {
                    state.round_tried = true;
                    state.round = crate::round::RoundShaders::compile(backend.borrow_mut().renderer());
                }
                state.animate();
                let borders = state.border_elements(&output);
                let blur_params = state.blur_params(&output);
                let hint = state.hint_element(&output);
                let corner = state.config.appearance.corner_radius.clamp(0, 64);
                let mut backend = backend.borrow_mut();
                let size = backend.window_size();
                if state.extcapture.as_ref().is_some_and(|e| e.wants()) {
                    let lock = state.lock_surfaces_for(&output);
                    let clean = crate::render::output_elements(backend.renderer(), &state.space, &output, None, borders.clone(), hint.clone(), Some((&mut state.blur, &blur_params)), state.round.as_ref().map(|r| (r, corner)), lock.as_deref());
                    if let Some(mut e) = state.extcapture.take() {
                        e.fulfil(backend.renderer(), &output, &state.space, size, output.current_scale().fractional_scale(), &clean, &clean);
                        state.extcapture = Some(e);
                    }
                }
                if state.screencopy.wants(&output) {
                    let frames = state.screencopy.take(&output);
                    let lock = state.lock_surfaces_for(&output);
                    let clean = crate::render::output_elements(backend.renderer(), &state.space, &output, None, borders.clone(), hint.clone(), Some((&mut state.blur, &blur_params)), state.round.as_ref().map(|r| (r, corner)), lock.as_deref());
                    state.screencopy.fulfil(frames, backend.renderer(), size, output.current_scale().fractional_scale(), &clean, &clean);
                }
                if let Some(kind) = state.pending_capture.take() {
                    let crop = state.capture_crop(kind);
                    let freeze = match kind {
                        crate::screenshot::CaptureKind::Freeze(p) => Some((state.freeze_windows(), p)),
                        _ => None,
                    };
                    let lock = state.lock_surfaces_for(&output);
                    let clean = crate::render::output_elements(backend.renderer(), &state.space, &output, None, borders.clone(), hint.clone(), Some((&mut state.blur, &blur_params)), state.round.as_ref().map(|r| (r, corner)), lock.as_deref());
                    if let Some(rgba) = crate::screenshot::grab(backend.renderer(), &clean, size, output.current_scale().fractional_scale()) {
                        if let Some((windows, pick)) = freeze {
                            crate::screenshot::write_freeze(rgba, size.w as u32, size.h as u32, windows, pick);
                        } else {
                            crate::screenshot::finish(rgba, size.w as u32, size.h as u32, crop, state.config.screenshots.dir.clone(), state.shot_tx.clone());
                        }
                    }
                }
                let age = backend.buffer_age().unwrap_or(0);
                let drawn: Option<Vec<Rectangle<i32, smithay::utils::Physical>>>;
                {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    let lock = state.lock_surfaces_for(&output);
                    let elements = crate::render::output_elements(renderer, &state.space, &output, None, borders.clone(), hint.clone(), Some((&mut state.blur, &blur_params)), state.round.as_ref().map(|r| (r, corner)), lock.as_deref());
                    let res = damage_tracker
                        .render_output(renderer, &mut framebuffer, age, &elements, [0.08, 0.08, 0.1, 1.0])
                        .unwrap();
                    drawn = res.damage.cloned();
                }
                // Only a frame with something new is shown, and only then do clients get their frame callbacks; otherwise
                // a client that redraws on every callback would keep us busy forever.
                if let Some(damage) = &drawn {
                    backend.submit(Some(damage)).unwrap();
                }
                // frame callbacks go out after every redraw, shown or not, so a client waiting for one never stalls
                for w in state.space.elements() {
                    w.send_frame(&output, state.start_time.elapsed(), Some(Duration::ZERO), |_, _| Some(output.clone()));
                }
                if state.animating() {
                    backend.window().request_redraw();
                }
                state.just_redrew = true;
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            _ => {}
        })
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

//! linux-dmabuf: lets GPU clients (EGL/Vulkan apps, browsers, kitty, ...) share buffers with us.

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        egl::EGLDevice,
        renderer::{gles::GlesRenderer, ImportDma},
    },
    delegate_dmabuf,
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::Vesper;

/// Advertise linux-dmabuf for `renderer`. Uses v4 feedback when the render node is known, v3 otherwise.
pub fn create_global(state: &mut Vesper, renderer: &GlesRenderer) {
    let formats = renderer.dmabuf_formats();
    let node = EGLDevice::device_for_display(renderer.egl_context().display())
        .ok()
        .and_then(|d| d.try_get_render_node().ok().flatten());
    let dh = state.display_handle.clone();
    match node.and_then(|n| DmabufFeedbackBuilder::new(n.dev_id(), formats.clone()).build().ok()) {
        Some(feedback) => {
            state.dmabuf_state.create_global_with_default_feedback::<Vesper>(&dh, &feedback);
        }
        None => {
            tracing::warn!("no render node for dmabuf feedback, advertising linux-dmabuf v3");
            state.dmabuf_state.create_global::<Vesper>(&dh, formats);
        }
    }
}

impl DmabufHandler for Vesper {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let ok = match (self.tty.as_mut(), self.dmabuf_importer.as_mut()) {
            (Some(tty), _) => tty.import_dmabuf(&dmabuf),
            (None, Some(import)) => import(&dmabuf),
            (None, None) => false,
        };
        if ok {
            let _ = notifier.successful::<Vesper>();
        } else {
            notifier.failed();
        }
    }
}

delegate_dmabuf!(Vesper);

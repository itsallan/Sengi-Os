//! linux-dmabuf: clients that render with the GPU hand over their buffers
//! without a copy. Only offered when the compositor itself renders with
//! GLES; the backend says which formats it takes and tries each import.

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::State;

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let accepted = self
            .dmabuf_import
            .as_mut()
            .is_some_and(|import| import(&dmabuf));
        if accepted {
            self.dmabufs_imported += 1;
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

delegate_dmabuf!(State);

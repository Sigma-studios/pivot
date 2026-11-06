//! Per-window live capture state (ext-image-copy-capture-v1).
//!
//! Pipeline per window: the compositor blits the window into one of our dmabufs (GPU -> GPU), we
//! attach that same buffer to a subsurface scaled with wp_viewport, and COSMIC composites it.
//! Nothing touches the CPU. Frames are only produced when the window actually changes, so the
//! effective rate tracks each app's own redraw rate, capped by the monitor refresh rate.

use std::time::Instant;

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm;
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
    ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
};

use crate::buffers::BusyFlag;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraints {
    pub width: u32,
    pub height: u32,
    pub shm_formats: Vec<wl_shm::Format>,
    pub dmabuf_device: Option<u64>,
    pub dmabuf_formats: Vec<(u32, Vec<u64>)>,
}

pub struct Slot {
    pub buffer: WlBuffer,
    /// True while the compositor holds this buffer (attached to the preview surface).
    pub busy: BusyFlag,
    pub width: u32,
    pub height: u32,
    pub stale: bool,
}

pub struct Capture {
    pub source: ExtImageCaptureSourceV1,
    pub session: ExtImageCopyCaptureSessionV1,
    /// Constraints being accumulated until the next `done`.
    pub incoming: Constraints,
    pub constraints: Option<Constraints>,
    pub slots: Vec<Option<Slot>>,
    /// Frame in flight and the slot it writes into. The protocol allows one at a time.
    pub frame: Option<(ExtImageCopyCaptureFrameV1, usize)>,
    /// A finished frame waiting for the panel to become visible.
    pub pending: Option<usize>,
    pub last_start: Option<Instant>,
    pub next_at: Option<Instant>,
    pub stopped: bool,
    pub failures: u32,
    pub has_frame: bool,
    /// Set after a dmabuf import failure: use shared memory for this window from now on.
    pub force_shm: bool,
}

impl Capture {
    pub fn new(source: ExtImageCaptureSourceV1, session: ExtImageCopyCaptureSessionV1) -> Self {
        Self {
            source,
            session,
            incoming: Constraints::default(),
            constraints: None,
            slots: Vec::new(),
            frame: None,
            pending: None,
            last_start: None,
            next_at: None,
            stopped: false,
            failures: 0,
            has_frame: false,
            force_shm: false,
        }
    }

    pub fn mark_all_stale(&mut self) {
        for s in self.slots.iter_mut().flatten() {
            s.stale = true;
        }
    }

    /// Destroy stale buffers nobody is using anymore.
    pub fn collect_stale(&mut self) {
        let in_frame = self.frame.as_ref().map(|f| f.1);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let free = slot.as_ref().is_some_and(|s| s.stale && !s.busy.get())
                && Some(i) != in_frame
                && Some(i) != self.pending;
            if free {
                if let Some(s) = slot.take() {
                    s.buffer.destroy();
                }
            }
        }
    }

    pub fn free_slot(&self) -> Option<usize> {
        let in_frame = self.frame.as_ref().map(|f| f.1);
        self.slots.iter().enumerate().find_map(|(i, s)| {
            let s = s.as_ref()?;
            (!s.stale && !s.busy.get() && Some(i) != in_frame && Some(i) != self.pending).then_some(i)
        })
    }

    pub fn live_slots(&self) -> usize {
        self.slots.iter().flatten().filter(|s| !s.stale).count()
    }

    pub fn insert_slot(&mut self, slot: Slot) -> usize {
        if let Some(i) = self.slots.iter().position(Option::is_none) {
            self.slots[i] = Some(slot);
            i
        } else {
            self.slots.push(Some(slot));
            self.slots.len() - 1
        }
    }

    pub fn aspect(&self) -> Option<f32> {
        let c = self.constraints.as_ref()?;
        (c.width > 0 && c.height > 0).then(|| c.width as f32 / c.height as f32)
    }

    pub fn destroy(self) {
        if let Some((f, _)) = self.frame {
            f.destroy();
        }
        self.session.destroy();
        self.source.destroy();
        for s in self.slots.into_iter().flatten() {
            s.buffer.destroy();
        }
    }
}

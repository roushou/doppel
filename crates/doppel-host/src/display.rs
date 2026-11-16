//! A running VM's display as a stream of NV12 frames, read from the memory
//! its VMM shares (see `DisplayLayout` in vmm.proto).

use std::os::fd::OwnedFd;
use std::sync::atomic::{Ordering, fence};

use doppel_machine::deadline::Deadline;
use doppel_machine::geometry::Size;
use doppel_protocol::display::{FrameRing, Plane, RingError, RingLayout, Slot, SlotRead};
use doppel_protocol::pb::vmm::v1 as vmm;

use crate::capabilities::Clock;
use crate::shared_memory::SharedMemory;
use crate::vmm::{VmmClient, VmmError};

/// Why the display can't be streamed.
#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error(transparent)]
    Vmm(#[from] VmmError),
    #[error("the VMM's display layout: {0}")]
    Layout(#[from] RingError),
    #[error("mapping the VMM's display memory: {0}")]
    Map(#[source] std::io::Error),
    #[error("the VMM announced slot {0}, outside its ring")]
    NoSuchSlot(u32),
}

/// A frame copied out of the ring: NV12 planes packed without row padding.
/// Reused from frame to frame, so reading allocates only when the size grows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Nv12Frame {
    width: u32,
    height: u32,
    luma: Vec<u8>,
    chroma: Vec<u8>,
}

impl Nv12Frame {
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// `width` bytes per row, `height` rows.
    #[must_use]
    pub fn luma(&self) -> &[u8] {
        &self.luma
    }

    /// Cb, Cr pairs: `width` bytes per row, `height / 2` rows.
    #[must_use]
    pub fn chroma(&self) -> &[u8] {
        &self.chroma
    }
}

/// A frame the VMM says is ready: which slot holds it, and its sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announcement {
    pub slot: u32,
    pub sequence: u64,
}

/// What a display's shared memory holds, mapped: frames are read from it
/// as they're announced, by whoever announces them (the VMM, or the daemon
/// passing the VMM's announcements on).
#[derive(Debug)]
pub struct SharedFrames {
    memory: SharedMemory,
    ring: FrameRing,
    points: Option<Size>,
}

impl SharedFrames {
    /// Maps `fd`, the memory the VMM shares, laid out as `layout`; `points`
    /// is the display's size in points, when the VMM said.
    ///
    /// # Errors
    /// [`DisplayError::Map`], or [`DisplayError::Layout`] when the layout
    /// doesn't fit the memory.
    pub fn map(
        layout: RingLayout,
        points: Option<Size>,
        fd: &OwnedFd,
    ) -> Result<Self, DisplayError> {
        let memory = SharedMemory::map(fd).map_err(DisplayError::Map)?;
        let ring = FrameRing::new(layout, memory.len() as u64)?;
        Ok(Self {
            memory,
            ring,
            points,
        })
    }

    /// The layout the VMM describes, in the ring's terms.
    #[must_use]
    pub fn layout(layout: &vmm::DisplayLayout) -> RingLayout {
        RingLayout {
            width_pixels: layout.width_pixels,
            height_pixels: layout.height_pixels,
            slot_count: layout.slot_count,
            slot_stride: layout.slot_stride,
            luma_offset: layout.luma_offset,
            luma_bytes_per_row: layout.luma_bytes_per_row,
            chroma_offset: layout.chroma_offset,
            chroma_bytes_per_row: layout.chroma_bytes_per_row,
        }
    }

    /// Frame size in pixels.
    #[must_use]
    pub const fn pixels(&self) -> (u32, u32) {
        (self.ring.width(), self.ring.height())
    }

    /// Display size in points, the coordinate space of input, when the
    /// VMM said.
    #[must_use]
    pub const fn points(&self) -> Option<Size> {
        self.points
    }

    /// Copies the announced frame into `frame`. [`SlotRead::Torn`] when the
    /// VMM reused its slot meanwhile: skip it, a newer one is coming.
    ///
    /// # Errors
    /// [`DisplayError::NoSuchSlot`].
    pub fn read(
        &self,
        announced: Announcement,
        frame: &mut Nv12Frame,
    ) -> Result<SlotRead, DisplayError> {
        let slot = self
            .ring
            .slot(announced.slot)
            .ok_or(DisplayError::NoSuchSlot(announced.slot))?;
        Ok(self.read_slot(slot, announced.sequence, frame))
    }

    fn read_slot(&self, slot: Slot, announced: u64, frame: &mut Nv12Frame) -> SlotRead {
        let header = |memory: &SharedMemory| memory.load_acquire(slot.header()).unwrap_or(0);
        let before = header(&self.memory);
        if before != announced {
            return SlotRead::Torn;
        }
        frame.width = self.ring.width();
        frame.height = self.ring.height();
        frame.luma.clear();
        frame.chroma.clear();
        for (plane, out) in [
            (Plane::Luma, &mut frame.luma),
            (Plane::Chroma, &mut frame.chroma),
        ] {
            for row in slot.rows(plane) {
                // Every row was checked against the mapping by FrameRing.
                if self.memory.copy_into(row, out).is_none() {
                    return SlotRead::Torn;
                }
            }
        }
        // The copies above must complete before the header is read again.
        fence(Ordering::Acquire);
        SlotRead::judge(announced, before, header(&self.memory))
    }
}

/// The display of one running VM, on a VMM connection of its own.
#[derive(Debug)]
pub struct DisplayStream<'c, C> {
    client: VmmClient<'c, C>,
    frames: SharedFrames,
}

impl<'c, C: Clock> DisplayStream<'c, C> {
    /// Opens the display on `client`, which then carries only this stream.
    ///
    /// # Errors
    /// See [`DisplayError`].
    pub fn open(mut client: VmmClient<'c, C>, by: Deadline) -> Result<Self, DisplayError> {
        let (layout, fd) = client.open_display(by)?;
        let points = layout
            .display_points
            .and_then(|size| Size::try_from(size).ok());
        let frames = SharedFrames::map(SharedFrames::layout(&layout), points, &fd)?;
        Ok(Self { client, frames })
    }

    /// Frame size in pixels.
    #[must_use]
    pub const fn pixels(&self) -> (u32, u32) {
        self.frames.pixels()
    }

    /// Display size in points: the coordinate space of input.
    #[must_use]
    pub const fn points(&self) -> Option<Size> {
        self.frames.points()
    }

    /// Waits for the next whole frame and copies it into `frame`; returns
    /// its sequence. Frames torn by the VMM reusing their slot are skipped.
    ///
    /// # Errors
    /// [`VmmError::Timeout`] when no frame comes by `by` (the guest's screen
    /// hasn't changed), [`VmmError::Closed`] once the VM stops.
    pub fn next_frame(&mut self, by: Deadline, frame: &mut Nv12Frame) -> Result<u64, DisplayError> {
        loop {
            let event = self.client.next_event(by)?;
            let Some(vmm::vmm_event::Kind::DisplayFrame(announced)) = event.kind else {
                continue;
            };
            let announced = Announcement {
                slot: announced.slot,
                sequence: announced.sequence,
            };
            match self.frames.read(announced, frame)? {
                SlotRead::Whole => return Ok(announced.sequence),
                SlotRead::Torn => {}
            }
        }
    }
}

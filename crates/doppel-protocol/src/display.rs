//! Where a VMM's display frames sit in the memory it shares: a ring of
//! slots, each a sequence header and two NV12 planes. The layout arrives
//! from another process, so it's checked once against the mapping's length:
//! past [`FrameRing::new`], every slot and row lies inside the mapping.

use std::num::NonZeroU32;
use std::ops::Range;

/// The layout as the VMM describes it, before it's checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLayout {
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub slot_count: u32,
    pub slot_stride: u64,
    pub luma_offset: u64,
    pub luma_bytes_per_row: u32,
    pub chroma_offset: u64,
    pub chroma_bytes_per_row: u32,
}

/// Why a layout doesn't fit its mapping.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    #[error("the display is {width}x{height} pixels: NV12 needs even, non-zero sides")]
    Size { width: u32, height: u32 },
    #[error("the ring has no slots")]
    NoSlots,
    #[error("a {plane} row of {bytes_per_row} bytes can't hold {width} pixels")]
    RowTooShort {
        plane: Plane,
        bytes_per_row: u32,
        width: u32,
    },
    #[error("the {0} plane overlaps the slot header or the other plane")]
    Overlap(Plane),
    #[error("a slot needs {needed} bytes but the stride is {stride}")]
    SlotTooSmall { needed: u64, stride: u64 },
    #[error("the ring needs {needed} bytes but {mapped} are shared")]
    PastMapping { needed: u64, mapped: u64 },
}

/// One of a frame's two planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// One byte per pixel.
    Luma,
    /// Interleaved Cb and Cr for each 2x2 block of pixels.
    Chroma,
}

impl std::fmt::Display for Plane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Luma => "luma",
            Self::Chroma => "chroma",
        })
    }
}

/// A checked ring layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRing {
    width: u32,
    height: u32,
    slot_count: NonZeroU32,
    slot_stride: usize,
    luma: PlaneLayout,
    chroma: PlaneLayout,
}

/// Where one plane's rows are, relative to its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneLayout {
    offset: usize,
    bytes_per_row: usize,
    /// The bytes of a row that hold pixels.
    row_len: usize,
    rows: usize,
}

/// Bytes at the start of a slot holding its frame's sequence.
pub const HEADER_LEN: usize = 8;

impl FrameRing {
    /// Checks `layout` against a mapping of `mapped` bytes.
    ///
    /// # Errors
    /// See [`RingError`].
    pub fn new(layout: RingLayout, mapped: u64) -> Result<Self, RingError> {
        let RingLayout {
            width_pixels: width,
            height_pixels: height,
            ..
        } = layout;
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(RingError::Size { width, height });
        }
        let slot_count = NonZeroU32::new(layout.slot_count).ok_or(RingError::NoSlots)?;
        for (plane, bytes_per_row) in [
            (Plane::Luma, layout.luma_bytes_per_row),
            (Plane::Chroma, layout.chroma_bytes_per_row),
        ] {
            // A chroma row holds width/2 pairs of bytes: `width` bytes too.
            if bytes_per_row < width {
                return Err(RingError::RowTooShort {
                    plane,
                    bytes_per_row,
                    width,
                });
            }
        }
        let luma_end =
            u64::from(layout.luma_bytes_per_row) * u64::from(height) + layout.luma_offset;
        let chroma_end =
            u64::from(layout.chroma_bytes_per_row) * u64::from(height / 2) + layout.chroma_offset;
        // Header, then luma, then chroma: the order the VMM writes them.
        if layout.luma_offset < HEADER_LEN as u64 {
            return Err(RingError::Overlap(Plane::Luma));
        }
        if layout.chroma_offset < luma_end {
            return Err(RingError::Overlap(Plane::Chroma));
        }
        if chroma_end > layout.slot_stride {
            return Err(RingError::SlotTooSmall {
                needed: chroma_end,
                stride: layout.slot_stride,
            });
        }
        let needed = layout
            .slot_stride
            .checked_mul(u64::from(slot_count.get()))
            .ok_or(RingError::PastMapping {
                needed: u64::MAX,
                mapped,
            })?;
        if needed > mapped {
            return Err(RingError::PastMapping { needed, mapped });
        }
        // Everything fits in `mapped`, which is a mapping's length: usize.
        let size =
            |n: u64| usize::try_from(n).map_err(|_| RingError::PastMapping { needed, mapped });
        Ok(Self {
            width,
            height,
            slot_count,
            slot_stride: size(layout.slot_stride)?,
            luma: PlaneLayout {
                offset: size(layout.luma_offset)?,
                bytes_per_row: size(layout.luma_bytes_per_row.into())?,
                row_len: size(width.into())?,
                rows: size(height.into())?,
            },
            chroma: PlaneLayout {
                offset: size(layout.chroma_offset)?,
                bytes_per_row: size(layout.chroma_bytes_per_row.into())?,
                row_len: size(width.into())?,
                rows: size((height / 2).into())?,
            },
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Where slot `index` is, or `None` past the ring's end.
    #[must_use]
    pub fn slot(&self, index: u32) -> Option<Slot> {
        (index < self.slot_count.get()).then(|| Slot {
            start: self.slot_stride * index as usize,
            luma: self.luma,
            chroma: self.chroma,
        })
    }
}

/// One slot of a checked ring: offsets into the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    start: usize,
    luma: PlaneLayout,
    chroma: PlaneLayout,
}

impl Slot {
    /// Offset of the slot's sequence header (8 bytes, aligned to 8).
    #[must_use]
    pub const fn header(&self) -> usize {
        self.start
    }

    /// The byte ranges of the plane's rows, top to bottom, pixels only.
    pub fn rows(&self, plane: Plane) -> impl Iterator<Item = Range<usize>> + use<> {
        let layout = match plane {
            Plane::Luma => self.luma,
            Plane::Chroma => self.chroma,
        };
        let first = self.start + layout.offset;
        (0..layout.rows).map(move |row| {
            let at = first + row * layout.bytes_per_row;
            at..at + layout.row_len
        })
    }
}

/// What a slot's header said around a read: the frame is whole only if it
/// held the announced sequence both before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRead {
    Whole,
    /// The VMM was writing the slot, or reused it during the read.
    Torn,
}

impl SlotRead {
    #[must_use]
    pub const fn judge(announced: u64, before: u64, after: u64) -> Self {
        if announced != 0 && before == announced && after == announced {
            Self::Whole
        } else {
            Self::Torn
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VMM's layout for a 2560x1600 display (as in the wire fixture).
    fn vmm_layout() -> RingLayout {
        RingLayout {
            width_pixels: 2560,
            height_pixels: 1600,
            slot_count: 3,
            slot_stride: 6_160_384,
            luma_offset: 64,
            luma_bytes_per_row: 2560,
            chroma_offset: 4_096_064,
            chroma_bytes_per_row: 2560,
        }
    }

    #[test]
    fn the_vmms_layout_is_accepted_and_every_row_lies_in_its_slot() {
        let ring = FrameRing::new(vmm_layout(), 3 * 6_160_384).unwrap();
        for index in 0..3 {
            let slot = ring.slot(index).unwrap();
            let start = 6_160_384 * index as usize;
            assert_eq!(slot.header(), start);
            let luma: Vec<_> = slot.rows(Plane::Luma).collect();
            let chroma: Vec<_> = slot.rows(Plane::Chroma).collect();
            assert_eq!((luma.len(), chroma.len()), (1600, 800));
            assert_eq!(luma[0], start + 64..start + 64 + 2560);
            assert!(luma.last().unwrap().end <= chroma[0].start);
            assert!(chroma.last().unwrap().end <= start + 6_160_384);
        }
        assert_eq!(ring.slot(3), None);
    }

    #[test]
    fn a_ring_past_the_mapping_is_refused() {
        assert_eq!(
            FrameRing::new(vmm_layout(), 3 * 6_160_384 - 1),
            Err(RingError::PastMapping {
                needed: 3 * 6_160_384,
                mapped: 3 * 6_160_384 - 1
            })
        );
    }

    #[test]
    fn layouts_that_would_read_outside_their_slot_are_refused() {
        let cases = [
            (
                RingLayout {
                    width_pixels: 2561,
                    ..vmm_layout()
                },
                RingError::Size {
                    width: 2561,
                    height: 1600,
                },
            ),
            (
                RingLayout {
                    slot_count: 0,
                    ..vmm_layout()
                },
                RingError::NoSlots,
            ),
            (
                RingLayout {
                    luma_bytes_per_row: 2559,
                    ..vmm_layout()
                },
                RingError::RowTooShort {
                    plane: Plane::Luma,
                    bytes_per_row: 2559,
                    width: 2560,
                },
            ),
            (
                RingLayout {
                    luma_offset: 4,
                    ..vmm_layout()
                },
                RingError::Overlap(Plane::Luma),
            ),
            (
                RingLayout {
                    chroma_offset: 4_096_000,
                    ..vmm_layout()
                },
                RingError::Overlap(Plane::Chroma),
            ),
            (
                RingLayout {
                    slot_stride: 6_000_000,
                    ..vmm_layout()
                },
                RingError::SlotTooSmall {
                    needed: 6_144_064,
                    stride: 6_000_000,
                },
            ),
        ];
        for (layout, expected) in cases {
            assert_eq!(
                FrameRing::new(layout, u64::MAX),
                Err(expected),
                "{layout:?}"
            );
        }
        // A stride whose ring would overflow u64.
        assert!(matches!(
            FrameRing::new(
                RingLayout {
                    slot_stride: u64::MAX,
                    ..vmm_layout()
                },
                u64::MAX
            ),
            Err(RingError::PastMapping { .. })
        ));
    }

    #[test]
    fn a_read_is_whole_only_if_the_header_held_the_announced_sequence_throughout() {
        assert_eq!(SlotRead::judge(7, 7, 7), SlotRead::Whole);
        assert_eq!(SlotRead::judge(7, 0, 7), SlotRead::Torn, "being written");
        assert_eq!(SlotRead::judge(7, 7, 10), SlotRead::Torn, "reused mid-read");
        assert_eq!(SlotRead::judge(7, 4, 4), SlotRead::Torn, "an older frame");
        assert_eq!(SlotRead::judge(0, 0, 0), SlotRead::Torn);
    }
}

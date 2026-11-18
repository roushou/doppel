//! A read-only mapping of memory another process writes: the VMM's display
//! frames. The one place in the host that touches raw pointers.
//!
//! Bytes may change while they're copied: callers check a sequence header
//! around each read (a seqlock, see `doppel_protocol::display::SlotRead`) and
//! drop what was torn.

#![expect(
    unsafe_code,
    reason = "mapping shared memory and reading it while another process writes it"
)]

use std::ffi::c_void;
use std::io;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::os::fd::OwnedFd;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use nix::sys::mman::{MapFlags, ProtFlags, mmap, munmap};
use nix::sys::stat::fstat;

#[derive(Debug)]
pub(crate) struct SharedMemory {
    start: NonNull<c_void>,
    len: NonZeroUsize,
}

// The mapping is read-only and owned by this value; reads go through
// atomics or plain copies the caller validates.
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    /// Maps all of `fd` read-only. The mapping outlives the descriptor.
    pub(crate) fn map(fd: &OwnedFd) -> io::Result<Self> {
        let size = fstat(fd)?.st_size;
        let len = usize::try_from(size)
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty shared memory"))?;
        // SAFETY: a fresh read-only shared mapping at an address the kernel
        // picks; nothing else in this process aliases it.
        let start = unsafe { mmap(None, len, ProtFlags::PROT_READ, MapFlags::MAP_SHARED, fd, 0) }?;
        Ok(Self { start, len })
    }

    pub(crate) const fn len(&self) -> usize {
        self.len.get()
    }

    /// The u64 at `offset` with acquire ordering, pairing with the VMM's
    /// release stores. `None` if it isn't 8-aligned and inside the mapping.
    pub(crate) fn load_acquire(&self, offset: usize) -> Option<u64> {
        if !offset.is_multiple_of(8) || offset.checked_add(8)? > self.len() {
            return None;
        }
        // SAFETY: in bounds and aligned (the mapping is page-aligned);
        // AtomicU64 has u64's layout, and the memory is only read here.
        let atomic = unsafe {
            &*self
                .start
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<AtomicU64>()
        };
        Some(atomic.load(Ordering::Acquire))
    }

    /// Appends the bytes in `range` to `out`. `None` if `range` leaves the
    /// mapping.
    pub(crate) fn copy_into(&self, range: Range<usize>, out: &mut Vec<u8>) -> Option<()> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        let count = range.end - range.start;
        out.reserve(count);
        // SAFETY: the source is inside the mapping, the destination is the
        // vector's reserved spare capacity, and the two don't overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.start.as_ptr().cast::<u8>().add(range.start),
                out.as_mut_ptr().add(out.len()),
                count,
            );
            out.set_len(out.len() + count);
        }
        Some(())
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `map` mapped; no reference into it
        // outlives `self`.
        // A failed unmap leaks the mapping: nothing else to do in a drop.
        let _ = unsafe { munmap(self.start, self.len.get()) };
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file_with(bytes: &[u8]) -> OwnedFd {
        let dir = std::env::temp_dir().join(format!("doppel-shm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}", bytes.len()));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        file.into()
    }

    #[test]
    fn reads_stay_inside_the_mapping() {
        let mut bytes = vec![0_u8; 64];
        bytes[8..16].copy_from_slice(&41_u64.to_le_bytes());
        bytes[16..20].copy_from_slice(b"abcd");
        let memory = SharedMemory::map(&file_with(&bytes)).unwrap();
        assert_eq!(memory.len(), 64);
        assert_eq!(memory.load_acquire(8), Some(41));
        assert_eq!(memory.load_acquire(12), None, "unaligned");
        assert_eq!(memory.load_acquire(64), None, "past the end");
        let mut out = b"x".to_vec();
        assert_eq!(memory.copy_into(16..20, &mut out), Some(()));
        assert_eq!(out, b"xabcd");
        assert_eq!(memory.copy_into(60..65, &mut out), None);
        assert_eq!(out, b"xabcd", "a refused copy appends nothing");
    }

    #[test]
    fn an_empty_object_is_refused() {
        assert!(SharedMemory::map(&file_with(&[])).is_err());
    }
}

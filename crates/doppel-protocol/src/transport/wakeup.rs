//! Ends a [`super::Channel`]'s wait for an event from another thread,
//! without closing the channel: the wait watches the [`Wakeup`] too, and
//! a [`Waker`] makes it readable. Once woken it stays woken.

use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;

/// What a wait watches besides its channel.
#[derive(Debug)]
pub struct Wakeup {
    watched: UnixStream,
    rung: UnixStream,
}

/// Wakes a [`Wakeup`] from any thread.
#[derive(Debug)]
pub struct Waker(UnixStream);

impl Wakeup {
    /// # Errors
    /// No socket pair could be made.
    pub fn new() -> io::Result<Self> {
        let (watched, rung) = UnixStream::pair()?;
        Ok(Self { watched, rung })
    }

    /// A handle that wakes this from any thread.
    ///
    /// # Errors
    /// The socket couldn't be duplicated.
    pub fn waker(&self) -> io::Result<Waker> {
        self.rung.try_clone().map(Waker)
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.watched.as_fd()
    }
}

impl Waker {
    pub fn wake(&self) {
        // The byte stays unread: every wait after this one ends at once too.
        // A failed write means the wakeup is gone, and nothing waits on it.
        let _ = (&self.0).write(&[1]);
    }
}

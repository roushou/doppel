//! What agents' sessions do to the machines, as they do it: the daemon
//! answers `WatchActivity` on a connection of its own, then sends an event
//! as each call on a machine gets its turn and as it ends, parsed once
//! into `doppel_machine::activity`; a person's own input is never streamed.

use std::os::unix::net::UnixStream;
use std::path::Path;

use doppel_desktop::deadline::Deadline;
use doppel_machine::activity::Activity;
use doppel_protocol::pb::daemon::v1 as daemon;

use crate::display::Stream;
use crate::{ClientError, Clock};

/// The activity stream, until the daemon closes it.
#[derive(Debug)]
pub struct ActivityStream<C> {
    channel: Stream<C>,
}

impl<C: Clock> ActivityStream<C> {
    /// Connects to the daemon on `socket` and starts watching.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn open(socket: &Path, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::from_connect)?;
        Self::over(stream, clock, by)
    }

    /// As [`ActivityStream::open`], on a connection already made.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn over(stream: UnixStream, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let (channel, daemon::Done {}, _) =
            Stream::open(stream, clock, daemon::WatchActivity {}, by)?;
        Ok(Self { channel })
    }

    /// The next call started or ended. Events this build can't read (no
    /// phase, a machine name that isn't one) are skipped.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] when none comes by `by`;
    /// [`ClientError::Closed`] once the daemon ends the stream.
    pub fn next(&mut self, by: Deadline) -> Result<Activity, ClientError> {
        loop {
            if let Some(daemon::daemon_event::Kind::Activity(activity)) = self.channel.event(by)?
                && let Ok(read) = Activity::try_from(activity)
            {
                return Ok(read);
            }
        }
    }
}

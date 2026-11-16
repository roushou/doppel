//! One connection, shared by callers on any number of threads: a
//! [`SharedChannel`] files each response under its request's id, and each
//! caller waits for its own until its deadline, or asks the daemon to stop
//! it when it is cancelled.

use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard, PoisonError};

use doppel_machine::deadline::Deadline;
use doppel_protocol::convert::RemoteError;
use doppel_protocol::exchange::{DaemonCall, EventLimit, ExchangeConfig};
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::daemon_response::Outcome;
use doppel_protocol::rpc::Rpc;
use doppel_protocol::transport::SharedChannel;

use crate::{Call, ClientError, Clock, HandedBack, MachineFailure};

#[derive(Debug)]
pub(crate) struct Connection<C> {
    channel: SharedChannel<daemon::DaemonFrame, C>,
}

impl<C: Clock> Connection<C> {
    /// Performs the `Hello` exchange on `stream` (the daemon speaks first).
    /// Nothing this connection asks reports events: any are passed over.
    pub(crate) fn open(stream: UnixStream, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let config = ExchangeConfig {
            events: EventLimit::Ignored,
            ..ExchangeConfig::STRICT
        };
        Ok(Self {
            channel: SharedChannel::open(stream, clock, config, by)?,
        })
    }

    /// Sends a request and waits for its answer. Failures the daemon
    /// reports come back as errors, never as answers. A cancel asks the
    /// daemon to stop it, and the caller still waits for its answer; a
    /// deadline that passes gives it up.
    ///
    /// Its hand-back note goes to the call's [`Notes`], with those of
    /// answers given up on: they are about their machine all the same.
    pub(crate) fn call<R: Rpc>(
        &self,
        request: R,
        call: Call<'_>,
    ) -> Result<R::Answer, ClientError> {
        let request = DaemonCall {
            caption: call.caption.to_owned(),
            why: call.why.to_owned(),
            operation: request.into_operation(),
        };
        let mut response = self.channel.call(request, call.by, call.cancel)?.response;
        if let Some(notes) = call.notes {
            let orphaned = self.channel.orphans().into_iter();
            notes.add(
                response
                    .handed_back
                    .take()
                    .into_iter()
                    .chain(orphaned.filter_map(|response| response.handed_back)),
            );
        }
        match response.outcome {
            Some(Outcome::Error(error)) => Err(RemoteError::from(error).into()),
            Some(Outcome::MachineFailure(failure)) => {
                Err(MachineFailure::from_wire(failure).into())
            }
            Some(outcome) => R::answered(outcome).ok_or(ClientError::WrongAnswer(R::NAME)),
            None => Err(ClientError::Protocol("response without outcome")),
        }
    }
}

/// The hand-back notes a caller's requests were sent: a person had control
/// of a machine its session used, so what it saw there may have changed.
/// Only a session's requests are sent any.
#[derive(Debug, Default)]
pub struct Notes(Mutex<Vec<daemon::HandedBack>>);

impl Notes {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The notes since last asked.
    ///
    /// # Errors
    /// [`ClientError::Parse`] for a note naming no valid machine.
    pub fn take(&self) -> Result<Vec<HandedBack>, ClientError> {
        Ok(std::mem::take(&mut *self.lock())
            .iter()
            .map(HandedBack::from_wire)
            .collect::<Result<_, _>>()?)
    }

    fn add(&self, notes: impl IntoIterator<Item = daemon::HandedBack>) {
        self.lock().extend(notes);
    }

    /// The notes are plain data: usable after any panic.
    fn lock(&self) -> MutexGuard<'_, Vec<daemon::HandedBack>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

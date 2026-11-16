//! doppeld: the daemon that owns a user's machines. It serves
//! `doppel.daemon.v1` on a unix socket in the runtime directory, one thread
//! per connection, and runs each request against [`Machines`].
//!
//! VMMs don't depend on it: they keep running when it stops, and it finds
//! them again by their sockets when it starts.

mod activity;
mod call_events;
mod display;
mod forks;
mod handle;
mod library;
mod operations;
mod recording;
mod requests;
mod scheduler;
mod serving;
mod session;
mod tokens;
mod web;

pub use forks::{EntropyMacs, MacError, MacSource};
pub use operations::{Acted, CallError};
pub use recording::RecordingError;
use scheduler::SessionId;
pub use serving::Stopper;

use std::collections::HashMap;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use doppel_host::capabilities::WallClock;
use doppel_host::files::Filesystem;
use doppel_host::machines::Machines;
use doppel_host::store::StoreError;
use doppel_protocol::exchange::Heard;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::transport::{Cancel, ChannelError, Replies, Requests};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Socket(#[from] StoreError),
    #[error("another daemon is already running on {0}")]
    AlreadyRunning(PathBuf),
    #[error("listening on {path}: {source}")]
    Listen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("accepting a connection: {0}")]
    Accept(#[source] io::Error),
}

/// The daemon's socket, bound and listening.
#[derive(Debug)]
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

impl Listener {
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// The machines, with a queue each: requests on one machine run one at a
/// time, in arrival order, and hold its agent session.
#[derive(Debug)]
pub struct Daemon<C, F> {
    machines: Machines<C, F>,
    /// Who may act: sessions, a person's control, each machine's line.
    scheduler: std::sync::Arc<scheduler::Waiting>,
    /// Each machine's agent session, while no turn holds it.
    agent_sessions: scheduler::AgentSessions<C>,
    /// Viewers watching what agents do.
    watchers: std::sync::Arc<activity::Watchers>,
    /// Where recent snapshots' elements are, for calls that act on one.
    elements: activity::Elements,
    /// Where fresh forks' MAC addresses come from.
    macs: Box<dyn MacSource>,
    /// The browser viewer's tokens.
    tokens: tokens::TokenFile,
    /// The browser viewer's listener.
    web: web::Web,
    /// Whether it serves or is stopping, and the connections it has open.
    serving: serving::Serving,
}

impl<C: WallClock, F: Filesystem> Daemon<C, F> {
    pub fn new(machines: Machines<C, F>, macs: Box<dyn MacSource>) -> Self {
        let tokens = tokens::TokenFile::in_root(machines.store().root().path());
        let web = web::Web::in_root(machines.store().root().path());
        Self {
            machines,
            scheduler: std::sync::Arc::default(),
            agent_sessions: scheduler::AgentSessions::default(),
            watchers: std::sync::Arc::default(),
            elements: activity::Elements::default(),
            macs,
            tokens,
            web,
            serving: serving::Serving::default(),
        }
    }

    /// Stops [`Self::serve`] from any thread, such as one that waits for
    /// the process's signals.
    #[must_use]
    pub fn stopper(&self) -> Stopper {
        self.serving.stopper()
    }

    #[must_use]
    pub fn machines(&self) -> &Machines<C, F> {
        &self.machines
    }
}

impl<C, F> Daemon<C, F>
where
    C: WallClock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// A client that sends nothing for this long while the daemon writes
    /// to it is dropped.
    const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

    /// Binds the daemon's socket. A socket file left by a daemon that
    /// exited is replaced; one that a daemon still answers on is not.
    ///
    /// # Errors
    /// [`DaemonError::AlreadyRunning`], or the socket can't be created.
    pub fn listen(&self) -> Result<Listener, DaemonError> {
        let socket = self.machines.store().daemon_socket()?;
        let path = socket.path().to_path_buf();
        match UnixStream::connect(&path) {
            Ok(_) => return Err(DaemonError::AlreadyRunning(path)),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(&path).map_err(|source| DaemonError::Listen {
                    path: path.clone(),
                    source,
                })?;
            }
            // Nothing there (NotFound), or something binding will report.
            Err(_) => {}
        }
        let listener = UnixListener::bind(&path).map_err(|source| DaemonError::Listen {
            path: path.clone(),
            source,
        })?;
        // The user's own processes only.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|source| DaemonError::Listen {
                path: path.clone(),
                source,
            })?;
        Ok(Listener { listener, path })
    }

    /// Takes a socket already bound at this store's daemon socket path,
    /// such as the one launchd binds from the job's plist. The socket can't
    /// show where it was bound (launchd's report its own internal path), so
    /// whoever passes it vouches for that.
    ///
    /// # Errors
    /// The store's socket path is invalid.
    pub fn adopt(&self, listener: UnixListener) -> Result<Listener, DaemonError> {
        let socket = self.machines.store().daemon_socket()?;
        Ok(Listener {
            listener,
            path: socket.path().to_path_buf(),
        })
    }

    /// Serves connections until a [`Stopper`] stops it, or accepting one
    /// fails. Either way it then closes every connection, so their
    /// sessions end and their recordings are completed, and returns once
    /// their requests have finished.
    ///
    /// # Errors
    /// [`DaemonError::Accept`].
    pub fn serve(&self, listener: &Listener) -> Result<(), DaemonError> {
        let (wake, woken) = UnixStream::pair().map_err(DaemonError::Accept)?;
        listener
            .listener
            .set_nonblocking(true)
            .map_err(DaemonError::Accept)?;
        std::thread::scope(|scope| {
            // The browser viewer's listener, while its setting is on.
            scope.spawn(|| self.web.supervise(self));
            let accepted = if self.serving.wake_with(wake) {
                self.accept(listener, &woken, scope)
            } else {
                Ok(())
            };
            self.serving.stopper().stop();
            self.web.end();
            accepted
        })
    }

    /// Accepts connections, each served on a thread of its own, until the
    /// daemon is stopping (`woken` says so) or accepting fails.
    fn accept<'scope>(
        &'scope self,
        listener: &Listener,
        woken: &UnixStream,
        scope: &'scope std::thread::Scope<'scope, '_>,
    ) -> Result<(), DaemonError> {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        loop {
            let mut ready = [
                PollFd::new(listener.listener.as_fd(), PollFlags::POLLIN),
                PollFd::new(woken.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut ready, PollTimeout::NONE) {
                Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                Err(errno) => return Err(DaemonError::Accept(errno.into())),
            }
            if self.serving.stopping() {
                eprintln!("doppeld: stopping");
                return Ok(());
            }
            match listener.listener.accept() {
                Ok((stream, _)) => match stream.set_nonblocking(false) {
                    Ok(()) => {
                        scope.spawn(move || self.serve_connection(stream));
                    }
                    Err(error) => eprintln!("doppeld: a connection was dropped: {error}"),
                },
                // Taken by nothing, or interrupted: wait again.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(DaemonError::Accept(error)),
            }
        }
    }

    /// Serves one client connected on `stream`, such as one end of a
    /// socket pair, until it closes the connection.
    pub fn serve_connection(&self, stream: UnixStream) {
        if let Err(error) = self.connection(stream, Reach::Local) {
            eprintln!("doppeld: a connection ended: {error}");
        }
    }

    /// Serves a browser that holds `token`, bridged by the gateway on
    /// `stream`: as any client, except for what only this Mac may ask.
    pub fn serve_remote(&self, stream: UnixStream, token: &doppel_gateway::Token) {
        let reach = Reach::Remote(token.id().clone());
        if let Err(error) = self.connection(stream, reach) {
            eprintln!(
                "doppeld: a browser's connection (token {}) ended: {error}",
                token.id()
            );
        }
    }

    /// One client: `Hello` both ways, then requests, each on a thread of
    /// its own, until it closes the connection or the daemon stops. Its
    /// requests still running are then cancelled, and its session ends.
    fn connection(&self, stream: UnixStream, reach: Reach) -> Result<(), ChannelError> {
        let connection = self.scheduler.connection();
        let closer = stream.try_clone().map_err(ChannelError::Io)?;
        let Some(admitted) = self.serving.admit(connection, closer) else {
            // Stopping: it closes as it goes.
            return Ok(());
        };
        let link = Link {
            reach,
            connection,
            replies: Replies::new(&stream, Self::WRITE_TIMEOUT)?,
            in_flight: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
        };
        link.replies.greet()?;
        let read = std::thread::scope(|scope| {
            let read = self.read_requests(Requests::new(stream), &link, scope);
            for cancel in link.in_flight().values() {
                cancel.cancel();
            }
            read
        });
        // A viewer that goes hands back what it controlled.
        self.scheduler
            .left(link.connection, self.machines.clock().now());
        if let Some(session) = *link.session() {
            let end = admitted.end();
            eprintln!("doppeld: {session} ended: {end}");
            if let Some(recordings) = self.scheduler.close(session, end) {
                recordings.finish(&self.machines);
            }
        }
        read
    }

    fn read_requests<'scope>(
        &'scope self,
        mut requests: Requests<daemon::DaemonFrame>,
        link: &'scope Link,
        scope: &'scope std::thread::Scope<'scope, '_>,
    ) -> Result<(), ChannelError> {
        while let Some(heard) = requests.hear()? {
            match heard {
                Heard::Request(request) => self.take_up(request, link, scope),
                Heard::Cancel(id) => {
                    // One that already finished has nothing left to stop.
                    if let Some(request) = link.in_flight().get(&id.get()) {
                        request.cancel();
                    }
                }
                // A newer client's frame: kept out, and the connection kept.
                Heard::Unreadable { bytes } => {
                    eprintln!("doppeld: ignored a frame this build can't read ({bytes} bytes)");
                }
                Heard::Unexpected(why) => eprintln!("doppeld: ignored {why}"),
            }
        }
        Ok(())
    }

    /// Answers `request` on a thread of its own, unless the client may not
    /// ask it from where it is.
    fn take_up<'scope>(
        &'scope self,
        request: daemon::DaemonRequest,
        link: &'scope Link,
        scope: &'scope std::thread::Scope<'scope, '_>,
    ) {
        let askers = request
            .operation
            .as_ref()
            .map_or(handle::Askers::Anyone, |operation| {
                handle::Traits::of(operation).askers
            });
        if let (Reach::Remote(_), handle::Askers::ThisMac) = (&link.reach, askers) {
            let refused = daemon::daemon_response::Outcome::Error(common::Error {
                kind: Some(common::error::Kind::Unsupported(common::Unsupported {
                    what: "only on this Mac's daemon socket, not from a browser".into(),
                })),
            });
            if let Err(error) = link.write(&Link::response(request.id, refused)) {
                eprintln!("doppeld: answering request {}: {error}", request.id);
            }
            return;
        }
        let cancel = Cancel::new();
        link.in_flight().insert(request.id, cancel.clone());
        // Noted here, as read: the request's thread may start after the
        // next one's.
        let call = self.scheduler.arrive(
            link.connection,
            Self::reserves(request.operation.as_ref()),
            cancel.clone(),
        );
        scope.spawn(move || {
            let id = request.id;
            let opens = request
                .operation
                .as_ref()
                .and_then(|operation| handle::Traits::of(operation).opens);
            if let Some(display) = opens {
                drop(call);
                self.serve_opened(id, display, &cancel, link);
                link.in_flight().remove(&id);
                return;
            }
            let (outcome, note) = self.answer(request, &call, link);
            // Answered: a session listing no longer shows it running.
            drop(call);
            link.in_flight().remove(&id);
            let mut response = Link::response(id, outcome);
            if let (Some(daemon::daemon_frame::Kind::Response(response)), Some(note)) =
                (&mut response.kind, note)
            {
                response.handed_back = Some(daemon::HandedBack {
                    machine: note.machine.to_string(),
                    held_millis: u64::try_from(note.held.as_millis()).unwrap_or(u64::MAX),
                });
            }
            if let Err(error) = link.write(&response) {
                eprintln!("doppeld: answering request {id}: {error}");
            }
        });
    }
}

/// Where a connection comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reach {
    /// This Mac's daemon socket: this user's own processes, or an SSH
    /// forward of it.
    Local,
    /// A browser, through the gateway, holding this token.
    Remote(doppel_gateway::TokenId),
}

/// The daemon serves the gateway's browsers.
impl<C, F> doppel_gateway::Connections for Daemon<C, F>
where
    C: WallClock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    fn now(&self) -> std::time::SystemTime {
        self.machines.clock().wall()
    }

    fn check(
        &self,
        secret: &doppel_gateway::TokenSecret,
    ) -> Result<doppel_gateway::Token, doppel_gateway::Refused> {
        let book = self
            .tokens
            .read()
            .map_err(|error| doppel_gateway::Refused::Book(error.to_string()))?;
        Ok(book.check(secret, self.now())?.clone())
    }

    fn serve(&self, stream: UnixStream, token: &doppel_gateway::Token) {
        self.serve_remote(stream, token);
    }

    fn video(
        &self,
        path: &doppel_gateway::VideoPath,
    ) -> Result<(std::fs::File, u64), doppel_gateway::NoVideo> {
        let no = |error: &dyn std::fmt::Display| doppel_gateway::NoVideo(error.to_string());
        let id: library::RecordingId = path.folder().parse().map_err(|error| no(&error))?;
        let file: library::VideoFile = path.file().parse().map_err(|error| no(&error))?;
        library::Library::in_root(self.machines.store().root().path())
            .video(id, &file)
            .map_err(|error| no(&error))
    }
}

/// What a connection's requests share: the way back to the client, how to
/// stop each one, and the session, once the client opens one.
#[derive(Debug)]
pub(crate) struct Link {
    /// Where the client is: what it may ask depends on it.
    reach: Reach,
    connection: scheduler::ConnectionId,
    replies: Replies<daemon::DaemonFrame>,
    in_flight: Mutex<HashMap<u64, Cancel>>,
    session: Mutex<Option<SessionId>>,
}

impl Link {
    /// Each lock guards plain data, consistent after any panic.
    fn in_flight(&self) -> MutexGuard<'_, HashMap<u64, Cancel>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) const fn connection(&self) -> scheduler::ConnectionId {
        self.connection
    }

    pub(crate) fn session(&self) -> MutexGuard<'_, Option<SessionId>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn response(id: u64, outcome: daemon::daemon_response::Outcome) -> daemon::DaemonFrame {
        daemon::DaemonFrame {
            kind: Some(daemon::daemon_frame::Kind::Response(
                daemon::DaemonResponse {
                    id,
                    handed_back: None,
                    outcome: Some(outcome),
                },
            )),
        }
    }

    /// One whole frame: responses finishing together don't interleave.
    fn write(&self, frame: &daemon::DaemonFrame) -> Result<(), ChannelError> {
        self.replies.send(frame)
    }

    /// One whole frame with `fd` passed along on its first byte, so the
    /// client receives the descriptor with this frame.
    fn write_with_fd(
        &self,
        frame: &daemon::DaemonFrame,
        fd: &impl AsFd,
    ) -> Result<(), ChannelError> {
        self.replies.send_with(frame, fd.as_fd())
    }

    /// Ends the connection both ways: the client reads its end, and this
    /// connection's reader stops.
    fn shut_down(&self) {
        self.replies.close();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use doppel_host::capabilities::{Clock as _, OsEntropy, SystemClock};
    use doppel_host::files::LocalFilesystem;
    use doppel_host::launcher::VmmBinary;
    use doppel_host::store::Store;
    use doppel_machine::bundle::{DataRoot, RuntimeDir};

    use super::*;

    /// Neither request below reaches the store.
    fn daemon() -> Daemon<SystemClock, LocalFilesystem> {
        let nowhere = PathBuf::from("/nonexistent/doppel-daemon-test");
        Daemon::new(
            Machines::new(
                Store::new(
                    DataRoot::new(nowhere.join("data")).unwrap(),
                    RuntimeDir::new(nowhere.join("run")).unwrap(),
                ),
                VmmBinary::new(nowhere.join("vmm")),
                SystemClock,
                LocalFilesystem,
            ),
            Box::new(EntropyMacs::new(OsEntropy)),
        )
    }

    /// A connection whose client never reads.
    fn link() -> Link {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        Link {
            reach: Reach::Local,
            connection: daemon().scheduler.connection(),
            replies: Replies::new(&ours, std::time::Duration::from_secs(10)).unwrap(),
            in_flight: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
        }
    }

    #[test]
    fn a_display_that_cannot_open_is_refused_on_its_connection() {
        let daemon = daemon();
        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| daemon.serve_connection(theirs));
            let by = doppel_machine::deadline::Deadline::after(
                SystemClock.now(),
                Duration::from_secs(5),
            );
            let refused =
                doppel_client::Display::over(ours, SystemClock, &"nope".parse().unwrap(), by);
            assert!(
                matches!(refused, Err(doppel_client::ClientError::Machine(_))),
                "{refused:?}"
            );
        });
    }

    #[test]
    fn an_invalid_machine_name_is_the_request_s_fault() {
        let daemon = daemon();
        let outcome = daemon.respond(
            Some(daemon::daemon_request::Operation::GetStatus(
                daemon::GetStatus {
                    name: "Not A Name".into(),
                },
            )),
            &daemon.scheduler.alone(),
            &link(),
        );
        let daemon::daemon_response::Outcome::Error(common::Error {
            kind: Some(common::error::Kind::InvalidArgument(argument)),
        }) = outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(argument.field, "name");
    }

    #[test]
    fn an_operation_this_build_doesn_t_know_is_unsupported() {
        assert!(matches!(
            {
                let daemon = daemon();
                daemon.respond(None, &daemon.scheduler.alone(), &link())
            },
            daemon::daemon_response::Outcome::Error(common::Error {
                kind: Some(common::error::Kind::Unsupported(_)),
            })
        ));
    }

    /// A request waiting its machine's turn is cancelled by its client,
    /// while the connection answers others meanwhile.
    #[test]
    fn requests_on_one_connection_run_concurrently_and_can_be_cancelled() {
        use doppel_host::capabilities::Clock as _;
        let daemon = daemon();
        let dev: doppel_machine::machine::MachineName = "dev".parse().unwrap();
        let holder = daemon.scheduler.alone();
        let (held, _) = holder
            .turn(&dev, &daemon.agent_sessions, || SystemClock.now())
            .unwrap();
        let (ours, theirs) = UnixStream::pair().unwrap();
        let soon = || {
            doppel_machine::deadline::Deadline::after(
                SystemClock.now(),
                std::time::Duration::from_secs(10),
            )
        };
        std::thread::scope(|scope| {
            scope.spawn(|| daemon.serve_connection(theirs));
            let client = doppel_client::Client::over(ours, SystemClock, soon()).unwrap();
            let cancel = Cancel::new();
            std::thread::scope(|inner| {
                let waiting = inner.spawn(|| {
                    client.pause(
                        &dev,
                        doppel_client::Call {
                            by: soon(),
                            caption: "",
                            why: "",
                            cancel: &cancel,
                            notes: None,
                        },
                    )
                });
                // Answered while the pause waits.
                let other = client.status(&"other".parse().unwrap(), soon());
                assert!(
                    matches!(other, Err(doppel_client::ClientError::Machine(_))),
                    "{other:?}"
                );
                assert!(!waiting.is_finished(), "dev's turn is held");
                cancel.cancel();
                match waiting.join().unwrap() {
                    Err(doppel_client::ClientError::Machine(failure)) => {
                        assert_eq!(failure.message(), "cancelled");
                        assert_eq!(
                            doppel_machine::outcome::Failure::certainty(&failure),
                            doppel_machine::outcome::Certainty::NotDone
                        );
                    }
                    other => panic!("{other:?}"),
                }
            });
            drop(held);
            // Closing the connection ends the daemon's side of it.
            drop(client);
        });
    }
}

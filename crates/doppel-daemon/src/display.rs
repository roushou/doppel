//! A machine's display, served on a connection: the VMM's shared memory is
//! passed to the viewer with the response, and each frame the VMM announces
//! is passed on as an event, so the viewer reads frames as it would from the
//! VMM itself, with no copy and no encode.

use doppel_host::capabilities::Clock;
use doppel_host::files::Filesystem;
use doppel_host::vmm::{VmmClient, VmmError};
use doppel_machine::machine::MachineName;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::daemon_response::Outcome;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::transport::{Cancel, Wakeup};

use crate::requests::Refusal;
use crate::{Daemon, Link};

impl<C, F> Daemon<C, F>
where
    C: Clock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// Serves `OpenDisplay` until the machine stops, the request is
    /// cancelled, or the viewer goes. When the machine stops, the connection
    /// is shut down: that is how the viewer learns the display ended.
    pub(crate) fn serve_display(
        &self,
        id: u64,
        request: &daemon::OpenDisplay,
        cancel: &Cancel,
        link: &Link,
    ) {
        let opened = Refusal::name("name", &request.name).and_then(|name| {
            let mut client = self
                .machines
                .machine(&name)
                .vmm(self.machines.clock(), self.machines.deadline())?;
            let (layout, fd) = client
                .open_display(self.machines.deadline())
                .map_err(|error| Refusal::from(doppel_host::machines::MachineError::from(error)))?;
            Ok((name, client, layout, fd))
        });
        let (name, mut client, layout, fd) = match opened {
            Ok(opened) => opened,
            Err(refusal) => {
                Self::respond_on(link, id, refusal.outcome());
                return;
            }
        };
        let response = Link::response(id, Outcome::DisplayLayout(layout));
        if let Err(error) = link.write_with_fd(&response, &fd) {
            eprintln!("doppeld: {name}: passing its display: {error}");
            return;
        }
        drop(fd);
        self.forward(id, &name, &mut client, cancel, link, |event| {
            if let vmm::vmm_event::Kind::DisplayFrame(frame) = event {
                Some(daemon::daemon_event::Kind::DisplayFrame(frame))
            } else {
                None
            }
        });
    }

    /// Serves `OpenEncodedDisplay` as [`Self::serve_display`] serves
    /// `OpenDisplay`, passing the VMM's encoded frames on as they come. A
    /// viewer that reads too slowly backs up to the VMM, which then skips
    /// to a new key frame for it.
    pub(crate) fn serve_encoded_display(
        &self,
        id: u64,
        request: &daemon::OpenEncodedDisplay,
        cancel: &Cancel,
        link: &Link,
    ) {
        let opened = Refusal::name("name", &request.name).and_then(|name| {
            let mut client = self
                .machines
                .machine(&name)
                .vmm(self.machines.clock(), self.machines.deadline())?;
            let opened = client
                .open_encoded_display(self.machines.deadline())
                .map_err(|error| Refusal::from(doppel_host::machines::MachineError::from(error)))?;
            Ok((name, client, opened))
        });
        let (name, mut client, opened) = match opened {
            Ok(opened) => opened,
            Err(refusal) => {
                Self::respond_on(link, id, refusal.outcome());
                return;
            }
        };
        if let Err(error) = link.write(&Link::response(id, Outcome::EncodedDisplay(opened))) {
            eprintln!("doppeld: {name}: opening its encoded display: {error}");
            return;
        }
        self.forward(id, &name, &mut client, cancel, link, |event| {
            if let vmm::vmm_event::Kind::EncodedFrame(frame) = event {
                Some(daemon::daemon_event::Kind::EncodedFrame(frame))
            } else {
                None
            }
        });
    }

    /// Passes the events of `client` that `pick` keeps to the viewer on
    /// `link`, as events of request `id`, until the machine stops, the
    /// request is cancelled, or the viewer goes. When the machine stops,
    /// the connection is shut down: that is how the viewer learns the
    /// display ended.
    fn forward<K: Clock>(
        &self,
        id: u64,
        name: &MachineName,
        client: &mut VmmClient<'_, K>,
        cancel: &Cancel,
        link: &Link,
        pick: impl Fn(vmm::vmm_event::Kind) -> Option<daemon::daemon_event::Kind>,
    ) {
        // A cancel wakes the wait for the next frame.
        let wakeup = match Wakeup::new() {
            Ok(wakeup) => wakeup,
            Err(error) => {
                eprintln!("doppeld: {name}: its display: {error}");
                link.shut_down();
                return;
            }
        };
        let _hook = match wakeup.waker() {
            Ok(waker) => cancel.on_cancel(move || waker.wake()),
            Err(error) => {
                eprintln!("doppeld: {name}: its display: {error}");
                link.shut_down();
                return;
            }
        };
        loop {
            match client.wait_event(&wakeup) {
                // Cancelled.
                Ok(None) => return,
                Ok(Some(vmm::VmmEvent {
                    kind: Some(kind), ..
                })) => {
                    // Other events aren't the viewer's.
                    let Some(kind) = pick(kind) else { continue };
                    let event = daemon::DaemonFrame {
                        kind: Some(daemon::daemon_frame::Kind::Event(daemon::DaemonEvent {
                            request: id,
                            kind: Some(kind),
                        })),
                    };
                    // The viewer went: nothing more to show.
                    if link.write(&event).is_err() {
                        return;
                    }
                }
                Ok(Some(vmm::VmmEvent { kind: None, .. })) => {}
                Err(VmmError::Closed) => {
                    link.shut_down();
                    return;
                }
                Err(error) => {
                    eprintln!("doppeld: {name}: its display: {error}");
                    link.shut_down();
                    return;
                }
            }
        }
    }

    fn respond_on(link: &Link, id: u64, outcome: Outcome) {
        if let Err(error) = link.write(&Link::response(id, outcome)) {
            eprintln!("doppeld: answering request {id}: {error}");
        }
    }

    /// Serves `WatchActivity`: Done, then each activity as it happens,
    /// until the request is cancelled or the viewer goes.
    fn serve_activity(&self, id: u64, cancel: &Cancel, link: &Link) {
        let (watcher, activity) = self.watchers.watch(self.machines.clock().now());
        // A cancel stops the watch, which ends the loop.
        let _hook = cancel.on_cancel({
            let watchers = std::sync::Arc::clone(&self.watchers);
            move || watchers.unwatch(watcher)
        });
        Self::respond_on(link, id, Outcome::Done(daemon::Done {}));
        loop {
            match activity.recv() {
                Ok(activity) => {
                    let event = daemon::DaemonFrame {
                        kind: Some(daemon::daemon_frame::Kind::Event(daemon::DaemonEvent {
                            request: id,
                            kind: Some(daemon::daemon_event::Kind::Activity(activity)),
                        })),
                    };
                    // The viewer went: nothing more to show.
                    if link.write(&event).is_err() {
                        return;
                    }
                }
                // Cancelled, or the daemon is going away.
                Err(_) => return,
            }
        }
    }

    /// Serves a stream request on its connection.
    pub(crate) fn serve_opened(&self, id: u64, opens: Opens<'_>, cancel: &Cancel, link: &Link) {
        match opens {
            Opens::Shared(request) => self.serve_display(id, request, cancel, link),
            Opens::Encoded(request) => self.serve_encoded_display(id, request, cancel, link),
            Opens::Activity => self.serve_activity(id, cancel, link),
        }
    }
}

/// A request served as a stream on its connection.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Opens<'r> {
    /// A display's frames in shared memory.
    Shared(&'r daemon::OpenDisplay),
    /// A display's frames encoded.
    Encoded(&'r daemon::OpenEncodedDisplay),
    /// What agents do, as they do it.
    Activity,
}

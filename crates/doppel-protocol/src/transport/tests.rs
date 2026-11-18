use std::io::{IoSlice, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use crate::exchange::ExchangeConfig;
use crate::exchange::Protocol;
use crate::frame::{FrameDecoder, FrameEncoder, MaxFrameLen, Received};
use crate::pb::common::v1 as common;
use crate::pb::vmm::v1 as vmm;
use doppel_machine::deadline::{Clock, Deadline};
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

use super::{Cancel, Channel, ChannelError, SharedChannel, Waited, Wakeup};
use crate::exchange::{DaemonCall, EventLimit};
use crate::pb::daemon::v1 as daemon;

#[derive(Debug, Clone, Copy)]
struct Now;

impl Clock for Now {
    #[expect(clippy::disallowed_methods, reason = "a test's real clock")]
    fn now(&self) -> Instant {
        Instant::now()
    }
}

fn after(budget: Duration) -> Deadline {
    Deadline::after(Now.now(), budget)
}

fn soon() -> Deadline {
    after(Duration::from_secs(5))
}

fn bytes(frame: &vmm::VmmFrame) -> Vec<u8> {
    FrameEncoder::new(MaxFrameLen::DEFAULT)
        .encode(frame)
        .unwrap()
        .to_vec()
}

fn hello() -> vmm::VmmFrame {
    vmm::VmmFrame {
        kind: Some(vmm::vmm_frame::Kind::Hello(common::Hello {
            protocol_version: vmm::VmmFrame::VERSION,
            ..common::Hello::default()
        })),
    }
}

fn done(id: u64) -> vmm::VmmFrame {
    vmm::VmmFrame {
        kind: Some(vmm::vmm_frame::Kind::Response(vmm::VmmResponse {
            id,
            outcome: Some(vmm::vmm_response::Outcome::Done(vmm::Done {})),
        })),
    }
}

fn stopped() -> vmm::VmmFrame {
    vmm::VmmFrame {
        kind: Some(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
            kind: Some(vmm::vmm_event::Kind::GuestStopped(
                vmm::GuestStopped::default(),
            )),
        })),
    }
}

fn pause() -> vmm::vmm_request::Operation {
    vmm::vmm_request::Operation::Pause(vmm::Pause {})
}

/// The VMM's side of a test: a blocking socket that reads our frames.
struct Peer {
    socket: UnixStream,
    decoder: FrameDecoder<vmm::VmmFrame>,
}

impl Peer {
    /// Greets, and reads the greeting back.
    fn greeting(socket: UnixStream) -> Self {
        let mut peer = Self {
            socket,
            decoder: FrameDecoder::new(MaxFrameLen::DEFAULT),
        };
        peer.send(&hello());
        assert!(matches!(
            peer.next().kind,
            Some(vmm::vmm_frame::Kind::Hello(_))
        ));
        peer
    }

    fn send(&mut self, frame: &vmm::VmmFrame) {
        self.socket.write_all(&bytes(frame)).unwrap();
    }

    /// Sends `frame` with `fd` attached to its first byte, as the VMM does.
    fn send_with(&mut self, frame: &vmm::VmmFrame, fd: &UnixStream) {
        let bytes = bytes(frame);
        let fds = [fd.as_raw_fd()];
        let sent = sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&bytes)],
            &[ControlMessage::ScmRights(&fds)],
            MsgFlags::empty(),
            None,
        )
        .unwrap();
        assert_eq!(sent, bytes.len());
    }

    fn next(&mut self) -> vmm::VmmFrame {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(Received::Recognized { message, .. }) = self.decoder.next_frame().unwrap() {
                return message;
            }
            let read = self.socket.read(&mut buffer).unwrap();
            assert_ne!(read, 0, "the client hung up");
            self.decoder.push(&buffer[..read]);
        }
    }

    /// The id of the next request.
    fn request(&mut self) -> u64 {
        match self.next().kind {
            Some(vmm::vmm_frame::Kind::Request(request)) => request.id,
            other => panic!("expected a request, got {other:?}"),
        }
    }
}

fn pair() -> (Channel<vmm::VmmFrame, Now>, Peer) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let greeter = thread::spawn(move || Peer::greeting(theirs));
    let channel = Channel::open(ours, Now, ExchangeConfig::STRICT, soon()).unwrap();
    (channel, greeter.join().unwrap())
}

#[test]
fn a_call_gets_its_response_and_events_wait_their_turn() {
    let (mut channel, mut peer) = pair();
    assert_eq!(channel.peer().protocol_version, vmm::VmmFrame::VERSION);
    let answering = thread::spawn(move || {
        let id = peer.request();
        peer.send(&stopped());
        peer.send(&done(id));
        peer
    });
    let answer = channel.call(pause(), soon()).unwrap();
    assert!(matches!(
        answer.response.outcome,
        Some(vmm::vmm_response::Outcome::Done(_))
    ));
    assert!(answer.attached.is_empty());
    assert!(channel.next_event(soon()).unwrap().is_some());
    drop(answering.join().unwrap());
}

#[test]
fn a_descriptor_comes_with_its_own_response_and_no_other() {
    let (mut channel, mut peer) = pair();
    let (stray, _stray_far) = UnixStream::pair().unwrap();
    let (memory, mut memory_far) = UnixStream::pair().unwrap();
    let answering = thread::spawn(move || {
        let id = peer.request();
        // An event with a descriptor nobody asked for, then the response.
        peer.send_with(&stopped(), &stray);
        peer.send_with(&done(id), &memory);
        peer
    });
    let mut answer = channel.call(pause(), soon()).unwrap();
    assert_eq!(answer.attached.len(), 1);
    // It is the response's: what's written to it comes out of `memory`'s far end.
    let mut received = UnixStream::from(answer.attached.remove(0));
    received.write_all(b"ours").unwrap();
    let mut read = [0u8; 4];
    memory_far.read_exact(&mut read).unwrap();
    assert_eq!(&read, b"ours");
    drop(answering.join().unwrap());
}

#[test]
fn a_call_ends_at_its_deadline_and_its_late_answer_is_dropped() {
    let (mut channel, mut peer) = pair();
    let started = Now.now();
    assert!(matches!(
        channel.call(pause(), after(Duration::from_millis(50))),
        Err(ChannelError::Timeout)
    ));
    assert!(Now.now() - started >= Duration::from_millis(50));
    // The late answer, then the next call's.
    let late = peer.request();
    peer.send(&done(late));
    let answering = thread::spawn(move || {
        let id = peer.request();
        peer.send(&done(id));
        peer
    });
    assert!(channel.call(pause(), soon()).is_ok());
    drop(answering.join().unwrap());
}

#[test]
fn waiting_for_an_event_that_does_not_come_is_not_an_error() {
    let (mut channel, _peer) = pair();
    assert!(
        channel
            .next_event(after(Duration::from_millis(20)))
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_interrupt_from_another_thread_ends_a_wait() {
    let (mut channel, peer) = pair();
    let interrupter = channel.interrupter().unwrap();
    let interrupting = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        interrupter.interrupt();
    });
    assert!(matches!(
        channel.call(pause(), soon()),
        Err(ChannelError::Closed)
    ));
    interrupting.join().unwrap();
    drop(peer);
}

#[test]
fn a_wakeup_ends_an_event_wait_and_leaves_the_channel_open() {
    let (mut channel, mut peer) = pair();
    let wakeup = Wakeup::new().unwrap();
    // An event already there comes first; then the wait times out.
    peer.send(&stopped());
    assert!(matches!(
        channel.wait_event(Some(soon()), &wakeup),
        Ok(Waited::Event(_))
    ));
    assert!(matches!(
        channel.wait_event(Some(after(Duration::from_millis(20))), &wakeup),
        Ok(Waited::TimedOut)
    ));
    let waker = wakeup.waker().unwrap();
    let waking = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        waker.wake();
    });
    assert!(matches!(
        channel.wait_event(None, &wakeup),
        Ok(Waited::WokenUp)
    ));
    waking.join().unwrap();
    // It stays woken, and the channel still works.
    assert!(matches!(
        channel.wait_event(None, &wakeup),
        Ok(Waited::WokenUp)
    ));
    assert!(channel.is_open());
    drop(peer);
}

#[test]
fn a_peer_that_hangs_up_closes_the_channel() {
    let (mut channel, peer) = pair();
    assert!(channel.is_open());
    drop(peer);
    assert!(matches!(
        channel.next_event(soon()),
        Err(ChannelError::Closed)
    ));
    assert!(!channel.is_open());
}

/// The daemon's side of a shared-channel test: requests and cancels in,
/// responses out, each frame whole.
struct DaemonPeer {
    socket: UnixStream,
    decoder: FrameDecoder<daemon::DaemonFrame>,
}

/// What a [`DaemonPeer`] read.
#[derive(Debug, PartialEq)]
enum Asked {
    Request(u64),
    Cancel(u64),
}

impl DaemonPeer {
    fn send(&mut self, frame: &daemon::DaemonFrame) {
        let bytes = FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(frame)
            .unwrap();
        self.socket.write_all(&bytes).unwrap();
    }

    fn next(&mut self) -> Option<daemon::DaemonFrame> {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(Received::Recognized { message, .. }) = self.decoder.next_frame().unwrap() {
                return Some(message);
            }
            match self.socket.read(&mut buffer).unwrap() {
                0 => return None,
                read => self.decoder.push(&buffer[..read]),
            }
        }
    }

    fn asked(&mut self) -> Asked {
        match self.next().and_then(|frame| frame.kind) {
            Some(daemon::daemon_frame::Kind::Request(request)) => Asked::Request(request.id),
            Some(daemon::daemon_frame::Kind::Cancel(cancel)) => Asked::Cancel(cancel.id),
            other => panic!("expected a request or a cancel, got {other:?}"),
        }
    }

    fn answer(&mut self, id: u64, held_millis: u64) {
        self.send(&daemon::DaemonFrame {
            kind: Some(daemon::daemon_frame::Kind::Response(
                daemon::DaemonResponse {
                    id,
                    handed_back: Some(daemon::HandedBack {
                        machine: "dev".into(),
                        held_millis,
                    }),
                    outcome: Some(daemon::daemon_response::Outcome::Done(daemon::Done {})),
                },
            )),
        });
    }
}

fn list() -> DaemonCall {
    DaemonCall {
        why: String::new(),
        caption: String::new(),
        operation: daemon::daemon_request::Operation::ListMachines(daemon::ListMachines {}),
    }
}

fn shared_pair() -> (SharedChannel<daemon::DaemonFrame, Now>, DaemonPeer) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let greeter = thread::spawn(move || {
        let mut peer = DaemonPeer {
            socket: theirs,
            decoder: FrameDecoder::new(MaxFrameLen::DEFAULT),
        };
        peer.send(&daemon::DaemonFrame {
            kind: Some(daemon::daemon_frame::Kind::Hello(common::Hello {
                protocol_version: daemon::DaemonFrame::VERSION,
                ..common::Hello::default()
            })),
        });
        assert!(matches!(
            peer.next().and_then(|frame| frame.kind),
            Some(daemon::daemon_frame::Kind::Hello(_))
        ));
        peer
    });
    let config = ExchangeConfig {
        events: EventLimit::Ignored,
        ..ExchangeConfig::STRICT
    };
    let channel = SharedChannel::open(ours, Now, config, soon()).unwrap();
    (channel, greeter.join().unwrap())
}

fn held(answer: &crate::exchange::Answer<daemon::DaemonResponse, std::os::fd::OwnedFd>) -> u64 {
    answer
        .response
        .handed_back
        .as_ref()
        .map_or(0, |note| note.held_millis)
}

#[test]
fn callers_on_two_threads_each_get_their_own_answer() {
    let (channel, mut peer) = shared_pair();
    thread::scope(|scope| {
        let first = scope.spawn(|| channel.call(list(), soon(), &Cancel::never()));
        let one = peer.asked();
        let second = scope.spawn(|| channel.call(list(), soon(), &Cancel::never()));
        let two = peer.asked();
        let (Asked::Request(one), Asked::Request(two)) = (one, two) else {
            panic!("expected two requests");
        };
        // Answered the other way round, each marked with its own id.
        peer.answer(two, two);
        peer.answer(one, one);
        assert_eq!(held(&first.join().unwrap().unwrap()), one);
        assert_eq!(held(&second.join().unwrap().unwrap()), two);
    });
}

#[test]
fn a_cancel_asks_the_peer_to_stop_and_waits_for_its_answer() {
    let (channel, mut peer) = shared_pair();
    let cancel = Cancel::new();
    thread::scope(|scope| {
        let waiting = scope.spawn(|| channel.call(list(), soon(), &cancel));
        let Asked::Request(id) = peer.asked() else {
            panic!("expected a request");
        };
        let asked_at = Now.now();
        cancel.cancel();
        assert_eq!(peer.asked(), Asked::Cancel(id));
        // Woken by the cancel, not by a poll's next turn.
        assert!(Now.now() - asked_at < Duration::from_millis(50));
        assert!(!waiting.is_finished(), "it still waits for the answer");
        peer.answer(id, 7);
        assert_eq!(held(&waiting.join().unwrap().unwrap()), 7);
    });
}

#[test]
fn a_late_answer_is_kept_as_an_orphan_for_what_it_says() {
    let (channel, mut peer) = shared_pair();
    assert!(matches!(
        channel.call(list(), after(Duration::from_millis(50)), &Cancel::never()),
        Err(ChannelError::Timeout)
    ));
    let Asked::Request(id) = peer.asked() else {
        panic!("expected a request");
    };
    // Asked to stop as it was given up.
    assert_eq!(peer.asked(), Asked::Cancel(id));
    peer.answer(id, 9);
    // A later call's answer comes after the orphan's.
    thread::scope(|scope| {
        let next = scope.spawn(|| channel.call(list(), soon(), &Cancel::never()));
        let Asked::Request(next_id) = peer.asked() else {
            panic!("expected a request");
        };
        peer.answer(next_id, 1);
        assert_eq!(held(&next.join().unwrap().unwrap()), 1);
    });
    let orphans = channel.orphans();
    assert_eq!(orphans.len(), 1);
    assert_eq!(
        orphans[0].handed_back.as_ref().map(|note| note.held_millis),
        Some(9)
    );
}

#[test]
fn a_peer_that_hangs_up_ends_every_wait() {
    let (channel, mut peer) = shared_pair();
    thread::scope(|scope| {
        let waiting = scope.spawn(|| channel.call(list(), soon(), &Cancel::never()));
        assert!(matches!(peer.asked(), Asked::Request(_)));
        drop(peer);
        assert!(matches!(waiting.join().unwrap(), Err(ChannelError::Closed)));
    });
    assert!(!channel.is_open());
    assert!(matches!(
        channel.call(list(), soon(), &Cancel::never()),
        Err(ChannelError::Closed)
    ));
}

#[test]
fn a_protocol_that_cannot_ask_gives_a_cancelled_call_up_at_once() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let greeter = thread::spawn(move || Peer::greeting(theirs));
    let channel: SharedChannel<vmm::VmmFrame, Now> =
        SharedChannel::open(ours, Now, ExchangeConfig::STRICT, soon()).unwrap();
    let mut peer = greeter.join().unwrap();
    let cancel = Cancel::new();
    thread::scope(|scope| {
        let waiting = scope.spawn(|| channel.call(pause(), soon(), &cancel));
        let id = peer.request();
        cancel.cancel();
        assert!(matches!(
            waiting.join().unwrap(),
            Err(ChannelError::Cancelled)
        ));
        peer.send(&done(id));
    });
}

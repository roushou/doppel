//! The display stream against a fake VMM: the layout and memory arrive with
//! the reply, frames are announced by event, and a frame the VMM is still
//! writing (or has reused) is skipped rather than shown torn.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::fs::{self, File};
use std::io::{IoSlice, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use doppel_desktop::deadline::Deadline;
use doppel_host::capabilities::{Clock, SystemClock};
use doppel_host::display::{DisplayError, DisplayStream, Nv12Frame};
use doppel_host::store::Store;
use doppel_host::vmm::{VmmClient, VmmError};
use doppel_machine::bundle::{DataRoot, RuntimeDir};
use doppel_protocol::frame::{FrameDecoder, FrameEncoder, MaxFrameLen, Received};
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::vmm::v1 as vmm;
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

/// 4x2 pixels, two slots of 32 bytes: header, luma rows padded to 8,
/// one chroma row of 4.
fn layout() -> vmm::DisplayLayout {
    vmm::DisplayLayout {
        width_pixels: 4,
        height_pixels: 2,
        display_points: Some(common::Size {
            width: 2.0,
            height: 1.0,
        }),
        slot_count: 2,
        slot_stride: 32,
        luma_offset: 8,
        luma_bytes_per_row: 8,
        chroma_offset: 24,
        chroma_bytes_per_row: 4,
    }
}

struct FakeVmm {
    stream: UnixStream,
    decoder: FrameDecoder<vmm::VmmFrame>,
}

impl FakeVmm {
    fn bytes(kind: vmm::vmm_frame::Kind) -> Vec<u8> {
        FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&vmm::VmmFrame { kind: Some(kind) })
            .unwrap()
            .to_vec()
    }

    fn send(&mut self, kind: vmm::vmm_frame::Kind) {
        self.stream.write_all(&Self::bytes(kind)).unwrap();
    }

    fn receive(&mut self) -> vmm::vmm_frame::Kind {
        loop {
            if let Some(Received::Recognized { message, .. }) = self.decoder.next_frame().unwrap() {
                return message.kind.unwrap();
            }
            let mut buf = [0; 4096];
            let n = self.stream.read(&mut buf).unwrap();
            assert!(n > 0, "the client hung up");
            self.decoder.push(&buf[..n]);
        }
    }

    fn accept(listener: &UnixListener) -> Self {
        let (stream, _) = listener.accept().unwrap();
        let mut vmm = Self {
            stream,
            decoder: FrameDecoder::new(MaxFrameLen::DEFAULT),
        };
        vmm.send(vmm::vmm_frame::Kind::Hello(common::Hello {
            protocol_version: <doppel_protocol::pb::vmm::v1::VmmFrame as doppel_protocol::exchange::Protocol>::VERSION,
            ..Default::default()
        }));
        assert!(matches!(vmm.receive(), vmm::vmm_frame::Kind::Hello(_)));
        vmm
    }

    /// Answers OpenDisplay with `layout`, passing `memory` along.
    fn open_display(&mut self, memory: &File) {
        let vmm::vmm_frame::Kind::Request(request) = self.receive() else {
            panic!("expected a request");
        };
        assert!(matches!(
            request.operation,
            Some(vmm::vmm_request::Operation::OpenDisplay(_))
        ));
        let reply = Self::bytes(vmm::vmm_frame::Kind::Response(vmm::VmmResponse {
            id: request.id,
            outcome: Some(vmm::vmm_response::Outcome::DisplayOpened(
                vmm::DisplayOpened {
                    layout: Some(layout()),
                },
            )),
        }));
        let fds = [memory.as_raw_fd()];
        let sent = sendmsg::<()>(
            self.stream.as_raw_fd(),
            &[IoSlice::new(&reply)],
            &[ControlMessage::ScmRights(&fds)],
            MsgFlags::empty(),
            None,
        )
        .unwrap();
        assert_eq!(sent, reply.len());
    }

    fn announce(&mut self, slot: u32, sequence: u64) {
        self.send(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
            kind: Some(vmm::vmm_event::Kind::DisplayFrame(vmm::DisplayFrame {
                slot,
                sequence,
            })),
        }));
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dd-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn store(&self) -> Store {
        Store::new(
            DataRoot::new(self.0.join("data")).unwrap(),
            RuntimeDir::new(self.0.join("run")).unwrap(),
        )
    }

    /// The ring's memory: a file both sides map.
    fn memory(&self) -> File {
        let path = self.0.join("ring");
        let mut file = File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(&[0; 64]).unwrap();
        file
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes a frame into `slot` of the ring, header last.
fn write_slot(memory: &mut File, slot: u64, sequence: u64, luma: [u8; 8], chroma: [u8; 4]) {
    let base = slot * 32;
    for (offset, bytes) in [
        (8, &luma[..4]),
        (16, &luma[4..]),
        (24, &chroma[..]),
        (0, &sequence.to_le_bytes()[..]),
    ] {
        memory.seek(SeekFrom::Start(base + offset)).unwrap();
        memory.write_all(bytes).unwrap();
    }
}

fn soon() -> Deadline {
    Deadline::after(SystemClock.now(), Duration::from_secs(5))
}

#[test]
fn whole_frames_are_copied_and_torn_ones_skipped() {
    let scratch = Scratch::new("frames");
    let store = scratch.store();
    let name = "work".parse().unwrap();
    let socket = store.socket(&name).unwrap();
    let listener = UnixListener::bind(socket.path()).unwrap();
    let mut memory = scratch.memory();

    let vmm_side = std::thread::spawn(move || {
        let mut vmm = FakeVmm::accept(&listener);
        vmm.open_display(&memory);
        // Slot 1 is announced while its header still says "being written".
        vmm.announce(1, 2);
        write_slot(&mut memory, 0, 3, [1, 2, 3, 4, 5, 6, 7, 8], [9, 10, 11, 12]);
        vmm.announce(0, 3);
        // Slot 1 announced again, but it holds an older frame by now.
        write_slot(&mut memory, 1, 4, [0; 8], [0; 4]);
        vmm.announce(1, 5);
        write_slot(&mut memory, 1, 6, [8, 7, 6, 5, 4, 3, 2, 1], [4, 3, 2, 1]);
        vmm.announce(1, 6);
        vmm
    });

    let client = VmmClient::connect(&socket, &SystemClock, soon()).unwrap();
    let mut stream = DisplayStream::open(client, soon()).unwrap();
    assert_eq!(stream.pixels(), (4, 2));
    assert_eq!(
        stream.points(),
        doppel_desktop::geometry::Size::new(2.0, 1.0).ok()
    );
    let mut frame = Nv12Frame::default();
    assert_eq!(stream.next_frame(soon(), &mut frame).unwrap(), 3);
    assert_eq!((frame.width(), frame.height()), (4, 2));
    assert_eq!(
        frame.luma(),
        [1, 2, 3, 4, 5, 6, 7, 8],
        "row padding dropped"
    );
    assert_eq!(frame.chroma(), [9, 10, 11, 12]);
    assert_eq!(stream.next_frame(soon(), &mut frame).unwrap(), 6);
    assert_eq!(frame.luma(), [8, 7, 6, 5, 4, 3, 2, 1]);
    drop(vmm_side.join().unwrap());
}

#[test]
fn a_slot_outside_the_ring_is_an_error() {
    let scratch = Scratch::new("slot");
    let store = scratch.store();
    let name = "work".parse().unwrap();
    let socket = store.socket(&name).unwrap();
    let listener = UnixListener::bind(socket.path()).unwrap();
    let memory = scratch.memory();
    let vmm_side = std::thread::spawn(move || {
        let mut vmm = FakeVmm::accept(&listener);
        vmm.open_display(&memory);
        vmm.announce(2, 1);
        vmm
    });
    let client = VmmClient::connect(&socket, &SystemClock, soon()).unwrap();
    let mut stream = DisplayStream::open(client, soon()).unwrap();
    assert!(matches!(
        stream.next_frame(soon(), &mut Nv12Frame::default()),
        Err(DisplayError::NoSuchSlot(2))
    ));
    drop(vmm_side.join().unwrap());
}

/// A VMM that announces frames and exits before the viewer reads them:
/// the viewer still gets them, then learns the VMM is gone. (macOS refuses
/// a socket timeout once the peer has closed its end, unread data or not.)
#[test]
fn frames_sent_before_the_vmm_stopped_are_still_read() {
    let scratch = Scratch::new("stopped");
    let store = scratch.store();
    let name = "work".parse().unwrap();
    let socket = store.socket(&name).unwrap();
    let listener = UnixListener::bind(socket.path()).unwrap();

    let (sent, stopped) = std::sync::mpsc::channel();
    let vmm_side = std::thread::spawn(move || {
        let mut vmm = FakeVmm::accept(&listener);
        vmm.announce(0, 1);
        vmm.announce(1, 2);
        drop(vmm);
        sent.send(()).unwrap();
    });

    let mut client = VmmClient::connect(&socket, &SystemClock, soon()).unwrap();
    stopped.recv().unwrap();
    vmm_side.join().unwrap();
    for sequence in [1, 2] {
        let event = client.next_event(soon()).unwrap();
        assert!(
            matches!(
                event.kind,
                Some(vmm::vmm_event::Kind::DisplayFrame(vmm::DisplayFrame { sequence: s, .. })) if s == sequence
            ),
            "{event:?}"
        );
    }
    let end = client.next_event(soon());
    assert!(matches!(end, Err(VmmError::Closed)), "{end:?}");
}

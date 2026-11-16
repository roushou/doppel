//! The daemon on a temporary store, through the real client: machines are
//! listed and their statuses read as `Machines` reads them, failures arrive
//! as failures, and a second daemon can't take the socket.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use doppel_client::{Client, ClientError, Listing, StopMode};
use doppel_daemon::{Daemon, DaemonError, EntropyMacs};
use doppel_host::capabilities::{Clock, SystemClock, WallClock};
use doppel_host::files::LocalFilesystem;
use doppel_host::launcher::VmmBinary;
use doppel_host::machines::Machines;
use doppel_host::store::Store;
use doppel_machine::bundle::{DataRoot, RuntimeDir};
use doppel_machine::deadline::Deadline;
use doppel_machine::lineage::{Lineage, Origin, RecordedLineage, Role};
use doppel_machine::machine::{Entropy, MachineName, NetworkSpec};
use doppel_machine::outcome::{Certainty, Failure};
use doppel_machine::vm::{NextStart, Status};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        // Short: the runtime directory holds unix socket paths.
        let dir = std::env::temp_dir().join(format!("dd-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn machines_dir(&self) -> PathBuf {
        DataRoot::new(self.0.join("data")).unwrap().machines_dir()
    }

    fn store(&self) -> Store {
        Store::new(
            DataRoot::new(self.0.join("data")).unwrap(),
            RuntimeDir::new(self.0.join("run")).unwrap(),
        )
    }

    fn daemon(&self) -> Daemon<SystemClock, LocalFilesystem> {
        Daemon::new(
            Machines::new(
                self.store(),
                VmmBinary::new(self.0.join("no-vmm")),
                SystemClock,
                LocalFilesystem,
            ),
            Box::new(EntropyMacs::new(Counting::default())),
        )
    }

    /// A machine whose spec is the golden fixture: valid, never started.
    fn machine(&self, name: &str, spec: &[u8]) {
        let path = self.store().bundle(&name.parse().unwrap()).image().spec;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, spec).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Entropy that makes the MACs 02:00:00:00:00:01, :02, and so on.
#[derive(Debug, Default)]
struct Counting(u8);

impl Entropy for Counting {
    type Error = std::convert::Infallible;
    fn fill(&mut self, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0 += 1;
        bytes.fill(0);
        if let Some(last) = bytes.last_mut() {
            *last = self.0;
        }
        Ok(())
    }
}

fn soon() -> Deadline {
    Deadline::after(SystemClock.now(), Duration::from_secs(5))
}

fn golden_spec() -> Vec<u8> {
    fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundle/spec.v1.bin"))
        .unwrap()
}

#[test]
fn clients_read_machines_through_the_daemon() {
    let scratch = Scratch::new("serve");
    scratch.machine("dev", &golden_spec());
    scratch.machine("broken", b"not a spec");
    // A golden image: its lineage comes through the list.
    let golden = Lineage::made(Origin::Installed).with_role(Role::Golden {
        template: "ready".parse().unwrap(),
    });
    let store = scratch.store();
    store
        .write_lineage(&store.bundle(&"dev".parse().unwrap()).lineage(), &golden)
        .unwrap();
    // An undecodable one is reported, not defaulted.
    fs::write(scratch.machines_dir().join("broken/lineage.pb"), b"\xff").unwrap();
    fs::create_dir_all(scratch.machines_dir().join("Not A Name")).unwrap();

    let daemon = Arc::new(scratch.daemon());
    let listener = daemon.listen().unwrap();
    let socket = listener.path().to_path_buf();
    {
        let daemon = Arc::clone(&daemon);
        std::thread::spawn(move || daemon.serve(&listener));
    }

    let client = Client::connect(&socket, SystemClock, soon()).unwrap();
    let mut listings = client.machines(soon()).unwrap();
    listings.sort_by_key(|listing| format!("{listing:?}"));
    let dev: MachineName = "dev".parse().unwrap();
    assert!(listings.contains(&Listing::Machine {
        name: dev.clone(),
        status: Ok(Status::Stopped(NextStart::ColdBoot)),
        lineage: RecordedLineage::Known(golden),
    }));
    assert!(listings.contains(&Listing::Unrecognized(OsString::from("Not A Name"))));
    let broken = listings
        .iter()
        .find_map(|listing| match listing {
            Listing::Machine {
                name,
                status: Err(failure),
                lineage: RecordedLineage::Unreadable(_),
            } if name.as_str() == "broken" => Some(failure.clone()),
            Listing::Machine { .. } | Listing::Unrecognized(_) => None,
        })
        .unwrap();
    assert!(broken.message().contains("spec"), "{broken}");
    assert_eq!(listings.len(), 3);

    // The same connection serves request after request.
    assert_eq!(
        client.status(&dev, soon()).unwrap(),
        Status::Stopped(NextStart::ColdBoot)
    );
    match client.status(&"nope".parse().unwrap(), soon()) {
        Err(ClientError::Machine(failure)) => {
            assert!(failure.message().contains("nope"), "{failure}");
        }
        other => panic!("{other:?}"),
    }

    // A second client is served alongside the first.
    let second = Client::connect(&socket, SystemClock, soon()).unwrap();
    assert_eq!(second.machines(soon()).unwrap().len(), 3);

    assert!(matches!(
        scratch.daemon().listen(),
        Err(DaemonError::AlreadyRunning(path)) if path == socket
    ));
}

#[test]
fn a_socket_left_by_a_daemon_that_exited_is_replaced() {
    let scratch = Scratch::new("stale");
    let socket = scratch
        .store()
        .daemon_socket()
        .unwrap()
        .path()
        .to_path_buf();
    // Bound and dropped: the file stays, nothing answers on it.
    drop(UnixListener::bind(&socket).unwrap());
    assert!(matches!(
        Client::connect(&socket, SystemClock, soon()),
        Err(ClientError::NoDaemon(_))
    ));
    assert!(scratch.daemon().listen().is_ok());
}

#[test]
fn without_a_daemon_the_client_says_so() {
    let scratch = Scratch::new("none");
    let socket = scratch
        .store()
        .daemon_socket()
        .unwrap()
        .path()
        .to_path_buf();
    assert!(matches!(
        Client::connect(&socket, SystemClock, soon()),
        Err(ClientError::NoDaemon(_))
    ));
}

/// Starts a daemon on the scratch store, serving until the test ends.
fn serve(scratch: &Scratch) -> PathBuf {
    let daemon = Arc::new(scratch.daemon());
    let listener = daemon.listen().unwrap();
    let socket = listener.path().to_path_buf();
    std::thread::spawn(move || daemon.serve(&listener));
    socket
}

fn machine_failure<T: std::fmt::Debug>(
    result: Result<T, ClientError>,
) -> doppel_client::MachineFailure {
    match result {
        Err(ClientError::Machine(failure)) => failure,
        other => panic!("{other:?}"),
    }
}

#[test]
fn lifecycle_requests_run_under_the_machine_s_claim() {
    let scratch = Scratch::new("life");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let client = Client::connect(&socket, SystemClock, soon()).unwrap();
    let dev: MachineName = "dev".parse().unwrap();
    let label = "clean".parse().unwrap();

    assert_eq!(client.data_root(soon()).unwrap(), scratch.0.join("data"));

    // Held by another process: the daemon gets "busy", as the CLI would.
    let machines = Machines::new(
        scratch.store(),
        VmmBinary::new(scratch.0.join("no-vmm")),
        SystemClock,
        LocalFilesystem,
    );
    let held = machines.claim(&dev, machines.deadline()).unwrap();
    let busy = machine_failure(client.pause(&dev, soon()));
    assert!(busy.message().contains("busy"), "{busy}");
    assert_eq!(busy.certainty(), Certainty::NotDone);
    drop(held);

    // Not running, so nothing to pause, resume, stop or checkpoint.
    for failure in [
        machine_failure(client.pause(&dev, soon())),
        machine_failure(client.resume(&dev, soon())),
        machine_failure(client.stop(&dev, StopMode::Force, soon())),
    ] {
        assert!(failure.message().contains("not running"), "{failure}");
    }
    let checkpoint = machine_failure(client.checkpoint(
        &dev,
        &label,
        doppel_machine::checkpoint::AfterCheckpoint::Resume,
        soon(),
    ));
    assert!(!checkpoint.message().is_empty());

    // No such checkpoint: nothing restored, nothing forked.
    let restore = machine_failure(client.restore(&dev, &label, soon()));
    assert!(restore.message().contains("clean"), "{restore}");
    let fork = machine_failure(client.fork(&dev, &label, &"copy".parse().unwrap(), soon()));
    assert!(fork.message().contains("clean"), "{fork}");
    assert_eq!(
        client.machines(soon()).unwrap().len(),
        1,
        "no fork appeared"
    );

    // The VMM binary is missing: the start fails, and the machine stays stopped.
    let start = machine_failure(client.start(&dev, doppel_client::Window::Hidden, soon()));
    assert!(!start.message().is_empty());
    assert_eq!(
        client.status(&dev, soon()).unwrap(),
        Status::Stopped(NextStart::ColdBoot)
    );
}

#[test]
fn a_session_is_listed_with_its_machines_until_its_connection_closes() {
    let scratch = Scratch::new("sess");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let agent = Client::connect(&socket, SystemClock, soon()).unwrap();
    let opened = agent
        .open_session(
            "doppel mcp (test)",
            doppel_client::Record::IntoStore,
            soon(),
        )
        .unwrap();
    // In the store, named by when it started and its id.
    let records = opened.recording.unwrap();
    assert!(records.is_dir(), "the daemon made the recording directory");
    let folder = records.file_name().unwrap().to_string_lossy().into_owned();
    assert!(folder.ends_with(&format!("Z-s{}", opened.id)), "{folder}");
    assert_eq!(
        records.parent().unwrap(),
        std::fs::canonicalize(scratch.0.join("data/recordings")).unwrap()
    );
    let dev: MachineName = "dev".parse().unwrap();
    let report = agent.report(&dev, soon()).unwrap();
    assert_eq!(report.status, Status::Stopped(NextStart::ColdBoot));
    assert_eq!(report.agent, doppel_machine::agent::AgentReport::NotRunning);
    assert!(report.checkpoints.is_empty());
    // Not running: nothing to record, and the failure is still answered.
    assert!(
        agent
            .screenshot(
                &dev,
                doppel_machine::input::Via::Host,
                doppel_machine::screen::ScreenshotSize::MatchPoints,
                soon(),
            )
            .is_err()
    );
    // A wait is answered even when it doesn't succeed.
    let wait = doppel_machine::wait::WaitSpec::new(
        doppel_machine::wait::TextCondition::Appears("saved".parse().unwrap()),
        Duration::from_millis(300),
    )
    .unwrap();
    let waited = agent.wait_for_text(&dev, &wait, soon()).unwrap();
    assert!(
        !matches!(waited, doppel_machine::wait::WaitOutcome::Satisfied { .. }),
        "{waited:?}"
    );

    let viewer = Client::connect(&socket, SystemClock, soon()).unwrap();
    let sessions = viewer.sessions(soon()).unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let session = &sessions[0];
    assert_eq!(session.client, "doppel mcp (test)");
    assert_eq!(session.machines, vec![dev]);
    assert_eq!(session.answered, 3);
    assert!(session.running.is_empty());
    // Every request is traced, the failed ones with why.
    let trace = std::fs::read_to_string(records.join("trace.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{trace}");
    assert_eq!(lines[0]["machine"], "dev");
    assert_eq!(lines[0]["outcome"], "done");
    assert_eq!(lines[1]["outcome"], "failed");
    assert!(lines[1]["error"].is_string(), "{trace}");
    // The wait was answered, but it failed: the machine isn't running.
    assert_eq!(lines[2]["outcome"], "failed", "{trace}");
    assert!(lines[2]["error"].is_string(), "{trace}");
    // No machine ran, so no segment started: its list is there, empty.
    let segments = std::fs::read_to_string(records.join("segments.jsonl")).unwrap();
    assert_eq!(segments, "");

    // A second session on one connection is refused.
    assert!(matches!(
        agent.open_session("again", doppel_client::Record::Off, soon()),
        Err(ClientError::Remote(_))
    ));

    drop(agent);
    let closed = (0..50).any(|_| {
        std::thread::sleep(Duration::from_millis(20));
        viewer.sessions(soon()).unwrap().is_empty()
    });
    assert!(closed, "the session ended with its connection");

    // Read back for replay: listed, and opened with its trace parsed.
    {
        use daemon::daemon_request::Operation;
        use daemon::daemon_response::Outcome;
        use doppel_protocol::pb::common::v1 as common;
        use doppel_protocol::pb::daemon::v1 as daemon;
        let mut reader = RawConnection::open(&socket);
        let Outcome::RecordingList(list) =
            reader.request(Operation::ListRecordings(daemon::ListRecordings {}))
        else {
            panic!("no recording list");
        };
        assert_eq!(list.recordings.len(), 1, "{list:?}");
        let summary = &list.recordings[0];
        assert_eq!(summary.id, folder);
        assert_eq!(summary.session, opened.id);
        // Its three calls, and the second session it was refused.
        assert_eq!((summary.videos, summary.calls), (0, 4));
        assert!(summary.machines.is_empty(), "no machine ran: no video");
        let Outcome::Recording(recording) =
            reader.request(Operation::OpenRecording(daemon::OpenRecording {
                id: folder.clone(),
            }))
        else {
            panic!("no recording");
        };
        let calls: Vec<(Option<&str>, i32)> = recording
            .trace
            .iter()
            .map(|line| match &line.line {
                Some(daemon::trace_line::Line::Call(call)) => {
                    (call.machine.as_deref(), call.outcome)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                (Some("dev"), daemon::TraceOutcome::Done as i32),
                (Some("dev"), daemon::TraceOutcome::Failed as i32),
                (Some("dev"), daemon::TraceOutcome::Failed as i32),
                (None, daemon::TraceOutcome::Refused as i32),
            ]
        );
        // Its calls saw no snapshot: none is kept, so asking is refused.
        assert!(recording.trace.iter().all(|line| matches!(
            &line.line,
            Some(daemon::trace_line::Line::Call(call)) if !call.seen
        )));
        let outcome = reader.request(Operation::OpenSeenSnapshot(daemon::OpenSeenSnapshot {
            recording: folder.clone(),
            line: 0,
        }));
        let Outcome::Error(common::Error {
            kind: Some(common::error::Kind::InvalidArgument(argument)),
            ..
        }) = &outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(argument.field, "line");
        // A name that isn't a recording's, and one that isn't there.
        for id in ["../data", "2026-01-01T00-00-00Z-s99"] {
            let outcome = reader.request(Operation::OpenRecording(daemon::OpenRecording {
                id: id.into(),
            }));
            let Outcome::Error(common::Error {
                kind: Some(common::error::Kind::InvalidArgument(argument)),
                ..
            }) = &outcome
            else {
                panic!("{id}: {outcome:?}");
            };
            assert_eq!(argument.field, "id");
        }
    }
}

#[test]
fn an_agent_s_calls_wait_while_a_person_has_the_machine_and_learn_of_it_after() {
    let scratch = Scratch::new("take");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let dev: MachineName = "dev".parse().unwrap();
    let agent = Client::connect(&socket, SystemClock, soon()).unwrap();
    agent
        .open_session("doppel mcp (test)", doppel_client::Record::Off, soon())
        .unwrap();
    agent.status(&dev, soon()).unwrap();
    let viewer = Client::connect(&socket, SystemClock, soon()).unwrap();
    let click = doppel_machine::input::Input::Key(doppel_machine::input::KeyChord {
        modifiers: Vec::new(),
        key: doppel_machine::input::Key::Character('a'),
    });
    // Input needs control first.
    assert!(matches!(
        viewer.host_input(&dev, &click, soon()),
        Err(ClientError::Remote(_))
    ));
    viewer.take_control(&dev, soon()).unwrap();
    let other = Client::connect(&socket, SystemClock, soon()).unwrap();
    assert!(matches!(
        other.take_control(&dev, soon()),
        Err(ClientError::Remote(_))
    ));
    // Outside a session, the CLI still gets answers.
    other.status(&dev, soon()).unwrap();
    let notes = doppel_client::Notes::new();
    let noted = doppel_client::Call {
        why: "",
        by: soon(),
        caption: "",
        cancel: &doppel_protocol::transport::Cancel::never(),
        notes: Some(&notes),
    };
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| agent.status(&dev, noted));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!waiting.is_finished(), "the agent's call waits");
        viewer.hand_back(&dev, soon()).unwrap();
        waiting.join().unwrap().unwrap();
    });
    // It came with the call that waited.
    let taken = notes.take().unwrap();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].machine, dev);
    assert!(taken[0].held >= Duration::from_millis(300));
    // Once only.
    agent.status(&dev, noted).unwrap();
    assert!(notes.take().unwrap().is_empty());
    // A viewer that goes hands back.
    viewer.take_control(&dev, soon()).unwrap();
    drop(viewer);
    let back = (0..50).any(|_| {
        std::thread::sleep(Duration::from_millis(20));
        other.take_control(&dev, soon()).is_ok()
    });
    assert!(
        back,
        "control went back when the viewer's connection closed"
    );
}

#[test]
fn a_paused_session_waits_until_resumed() {
    let scratch = Scratch::new("pause");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let dev: MachineName = "dev".parse().unwrap();
    let agent = Client::connect(&socket, SystemClock, soon()).unwrap();
    let session = agent
        .open_session("doppel mcp (test)", doppel_client::Record::Off, soon())
        .unwrap();
    let cli = Client::connect(&socket, SystemClock, soon()).unwrap();
    cli.pause_session(session.id, soon()).unwrap();
    assert_eq!(
        cli.sessions(soon()).unwrap()[0].state,
        doppel_client::SessionStatus::Paused
    );
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| agent.status(&dev, soon()));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!waiting.is_finished(), "a paused session's call waits");
        cli.resume_session(session.id, soon()).unwrap();
        waiting.join().unwrap().unwrap();
    });
    assert!(matches!(
        cli.resume_session(session.id, soon()),
        Err(ClientError::Remote(_))
    ));
}

/// A VMM on a machine's socket that serves one encoded display: it answers
/// the daemon's probe, then opens the display, sends `frames` among events
/// that aren't the viewer's, and stops.
struct FakeVmm {
    stream: std::os::unix::net::UnixStream,
    decoder: doppel_protocol::frame::FrameDecoder<vmm::VmmFrame>,
}

impl FakeVmm {
    fn send(&mut self, kind: vmm::vmm_frame::Kind) {
        use std::io::Write as _;
        let bytes =
            doppel_protocol::frame::FrameEncoder::new(doppel_protocol::frame::MaxFrameLen::DEFAULT)
                .encode(&vmm::VmmFrame { kind: Some(kind) })
                .unwrap();
        self.stream.write_all(&bytes).unwrap();
    }

    /// The next frame, or none once the peer hung up.
    fn receive(&mut self) -> Option<vmm::vmm_frame::Kind> {
        use std::io::Read as _;
        loop {
            if let Some(doppel_protocol::frame::Received::Recognized { message, .. }) =
                self.decoder.next_frame().unwrap()
            {
                return message.kind;
            }
            let mut buffer = [0; 4096];
            let read = self.stream.read(&mut buffer).unwrap();
            if read == 0 {
                return None;
            }
            self.decoder.push(&buffer[..read]);
        }
    }

    fn accept(listener: &UnixListener) -> Self {
        let mut vmm = Self {
            stream: listener.accept().unwrap().0,
            decoder: doppel_protocol::frame::FrameDecoder::new(
                doppel_protocol::frame::MaxFrameLen::DEFAULT,
            ),
        };
        vmm.send(vmm::vmm_frame::Kind::Hello(
            doppel_protocol::pb::common::v1::Hello {
                protocol_version: doppel_protocol::PROTOCOL_VERSION,
                ..Default::default()
            },
        ));
        assert!(matches!(
            vmm.receive(),
            Some(vmm::vmm_frame::Kind::Hello(_))
        ));
        vmm
    }

    fn serve_encoded(listener: &UnixListener, frames: Vec<vmm::EncodedFrame>) {
        // The daemon's probe that the machine runs.
        let mut probe = Self::accept(listener);
        assert!(probe.receive().is_none());
        let mut vmm = Self::accept(listener);
        let Some(vmm::vmm_frame::Kind::Request(request)) = vmm.receive() else {
            panic!("expected a request");
        };
        assert!(matches!(
            request.operation,
            Some(vmm::vmm_request::Operation::OpenEncodedDisplay(_))
        ));
        vmm.send(vmm::vmm_frame::Kind::Response(vmm::VmmResponse {
            id: request.id,
            outcome: Some(vmm::vmm_response::Outcome::EncodedDisplayOpened(
                vmm::EncodedDisplayOpened {
                    width_pixels: 2560,
                    height_pixels: 1600,
                    display_points: Some(doppel_protocol::pb::common::v1::Size {
                        width: 1280.0,
                        height: 800.0,
                    }),
                },
            )),
        }));
        for frame in frames {
            vmm.send(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::DisplayFrame(vmm::DisplayFrame {
                    slot: 0,
                    sequence: 1,
                })),
            }));
            vmm.send(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::EncodedFrame(frame)),
            }));
        }
        // The machine stops.
    }
}

use doppel_protocol::convert::RemoteError;
use doppel_protocol::pb::vmm::v1 as vmm;

#[test]
fn an_encoded_display_passes_the_vmm_s_frames_on_in_order_until_the_machine_stops() {
    use doppel_client::{EncodedDisplay, EncodedFrame, EncodedSize, FrameKind};
    let scratch = Scratch::new("enc");
    scratch.machine("dev", &golden_spec());
    let dev: MachineName = "dev".parse().unwrap();
    let listener = UnixListener::bind(scratch.store().socket(&dev).unwrap().path()).unwrap();
    let wire = |sequence, key: bool, data: &[u8]| vmm::EncodedFrame {
        sequence,
        key,
        parameter_sets: if key {
            vec![vec![0x67, 1].into(), vec![0x68, 2].into()]
        } else {
            Vec::new()
        },
        data: data.to_vec().into(),
    };
    let sent = vec![
        wire(7, true, &[0, 0, 0, 1, 0x65]),
        wire(8, false, &[0, 0, 0, 1, 0x41]),
        wire(12, true, &[0, 0, 0, 1, 0x65]),
    ];
    let vmm = std::thread::spawn(move || FakeVmm::serve_encoded(&listener, sent));
    let socket = serve(&scratch);

    let (mut display, size) = EncodedDisplay::open(&socket, SystemClock, &dev, soon()).unwrap();
    assert_eq!(
        size,
        EncodedSize {
            width_pixels: 2560,
            height_pixels: 1600,
            points: doppel_machine::geometry::Size::new(1280.0, 800.0).ok(),
        }
    );
    let key = |sequence| EncodedFrame {
        sequence,
        kind: FrameKind::Key {
            parameter_sets: vec![vec![0x67, 1], vec![0x68, 2]],
        },
        data: vec![0, 0, 0, 1, 0x65],
    };
    let received: Vec<_> = (0..3)
        .map(|_| display.next_frame(soon()).unwrap())
        .collect();
    assert_eq!(
        received,
        [
            key(7),
            EncodedFrame {
                sequence: 8,
                kind: FrameKind::Delta,
                data: vec![0, 0, 0, 1, 0x41],
            },
            key(12),
        ]
    );
    vmm.join().unwrap();
    let end = display.next_frame(soon());
    assert!(matches!(end, Err(ClientError::Closed)), "{end:?}");
}

#[test]
fn forks_run_under_the_next_free_names_if_they_fit() {
    let scratch = Scratch::new("forks");
    let spec = golden_spec();
    scratch.machine("base", &spec);
    scratch.machine("base-clean-1", &spec);
    let store = scratch.store();
    let base = store.bundle(&"base".parse().unwrap());
    let checkpoint = base.checkpoint(&"clean".parse().unwrap());
    fs::create_dir_all(checkpoint.image.spec.parent().unwrap()).unwrap();
    fs::write(&checkpoint.image.spec, &spec).unwrap();
    fs::write(&checkpoint.image.disk, "cp-disk").unwrap();
    fs::write(&checkpoint.image.aux, "cp-aux").unwrap();
    fs::write(&checkpoint.memory, "cp-memory").unwrap();
    let socket = serve(&scratch);
    let client = Client::connect(&socket, SystemClock, soon()).unwrap();
    let name: MachineName = "base".parse().unwrap();
    let label = "clean".parse().unwrap();

    // Apple runs two VMs at a time: three are refused before any is made.
    match client.run_forks(&name, &label, 3, soon()) {
        Err(ClientError::Remote(RemoteError::InvalidState(_))) => {}
        other => panic!("{other:?}"),
    }
    match client.run_forks(&name, &label, 0, soon()) {
        Err(ClientError::Remote(RemoteError::InvalidArgument { field, .. })) => {
            assert_eq!(field, "count");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        client.machines(soon()).unwrap().len(),
        2,
        "nothing was made"
    );

    // Two fit: made under the next free names with their own MACs, and
    // each start's failure (no VMM here) is its own.
    let runs = client.run_forks(&name, &label, 2, soon()).unwrap();
    let names: Vec<String> = runs.iter().map(|run| run.name.to_string()).collect();
    assert_eq!(names, ["base-clean-2", "base-clean-3"]);
    for (run, mac) in runs.iter().zip(["02:00:00:00:00:01", "02:00:00:00:00:02"]) {
        let failure = run.desktop.as_ref().unwrap_err();
        assert!(!failure.message().is_empty());
        let fork = store.bundle(&run.name);
        assert_eq!(fs::read_to_string(fork.image().disk).unwrap(), "cp-disk");
        assert_eq!(
            store.read_spec(&fork).unwrap().network(),
            NetworkSpec::Nat {
                mac: mac.parse().unwrap()
            }
        );
        assert_eq!(
            client.status(&run.name, soon()).unwrap(),
            Status::Stopped(NextStart::ColdBoot)
        );
    }
    assert_eq!(client.machines(soon()).unwrap().len(), 4);
}

/// Two requests on one machine, sent back to back on one connection, run
/// in that order: the fork takes a name, so the run of forks after it
/// takes the next one. They're sent while the session is paused, so both
/// are let go at once when it resumes; many times over, since a race
/// would show only now and then.
#[test]
fn a_connection_s_requests_on_one_machine_run_in_the_order_sent() {
    use doppel_protocol::pb::daemon::v1 as daemon;
    let scratch = Scratch::new("order");
    let spec = golden_spec();
    scratch.machine("dev", &spec);
    let checkpoint = scratch
        .store()
        .bundle(&"dev".parse().unwrap())
        .checkpoint(&"clean".parse().unwrap());
    fs::create_dir_all(checkpoint.image.spec.parent().unwrap()).unwrap();
    fs::write(&checkpoint.image.spec, &spec).unwrap();
    fs::write(&checkpoint.image.disk, "cp-disk").unwrap();
    fs::write(&checkpoint.image.aux, "cp-aux").unwrap();
    fs::write(&checkpoint.memory, "cp-memory").unwrap();
    let socket = serve(&scratch);
    let mut connection = RawConnection::open(&socket);
    let daemon::daemon_response::Outcome::SessionOpened(opened) = connection.request(
        daemon::daemon_request::Operation::OpenSession(daemon::OpenSession {
            client: "doppel mcp (test)".into(),
            ..Default::default()
        }),
    ) else {
        panic!("no session");
    };
    let cli = Client::connect(&socket, SystemClock, soon()).unwrap();
    let request = |id, operation| {
        daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
            why: String::new(),
            id,
            caption: String::new(),
            operation: Some(operation),
        })
    };
    for round in 1..=30_u64 {
        let forked = format!("dev-clean-{}", 2 * round - 1);
        cli.pause_session(opened.session, soon()).unwrap();
        connection.send(request(
            2 * round,
            daemon::daemon_request::Operation::ForkMachine(daemon::ForkMachine {
                source: "dev".into(),
                label: "clean".into(),
                into: forked.clone(),
            }),
        ));
        connection.send(request(
            2 * round + 1,
            daemon::daemon_request::Operation::RunForks(daemon::RunForks {
                name: "dev".into(),
                label: "clean".into(),
                count: 1,
            }),
        ));
        std::thread::sleep(Duration::from_millis(30));
        cli.resume_session(opened.session, soon()).unwrap();
        let mut answers = std::collections::BTreeMap::new();
        while answers.len() < 2 {
            if let daemon::daemon_frame::Kind::Response(response) = connection.receive() {
                answers.insert(response.id, response.outcome.unwrap());
            }
        }
        assert!(
            matches!(
                answers[&(2 * round)],
                daemon::daemon_response::Outcome::Done(_)
            ),
            "round {round}: {:?}",
            answers[&(2 * round)]
        );
        let daemon::daemon_response::Outcome::ForksRun(run) = &answers[&(2 * round + 1)] else {
            panic!("round {round}: {:?}", answers[&(2 * round + 1)]);
        };
        let names: Vec<&str> = run.forks.iter().map(|fork| fork.name.as_str()).collect();
        assert_eq!(
            names,
            [format!("dev-clean-{}", 2 * round)],
            "round {round}: the run came after the fork into {forked}"
        );
    }
}

#[test]
fn tokens_are_kept_by_digest_until_revoked() {
    use std::os::unix::fs::PermissionsExt as _;

    use doppel_gateway::{TokenBook, TokenSecret, Ttl};

    let scratch = Scratch::new("tokens");
    let socket = serve(&scratch);
    let client = Client::connect(&socket, SystemClock, soon()).unwrap();
    let phone = TokenSecret::from_bytes([1; 32]);
    let laptop = TokenSecret::from_bytes([2; 32]);
    let issued = client
        .issue_token(
            &phone.digest(),
            Ttl::DEFAULT,
            Some(&"phone".parse().unwrap()),
            soon(),
        )
        .unwrap();
    assert_eq!(issued.id, phone.digest().id());
    assert_eq!(issued.label, Some("phone".parse().unwrap()));
    assert_eq!(
        issued.expires.duration_since(issued.issued).unwrap(),
        Ttl::DEFAULT.as_duration()
    );
    client
        .issue_token(&laptop.digest(), Ttl::DEFAULT, None, soon())
        .unwrap();

    // The same token twice, and values out of range, change nothing.
    assert!(matches!(
        client.issue_token(&phone.digest(), Ttl::DEFAULT, None, soon()),
        Err(ClientError::Remote(RemoteError::InvalidState(_)))
    ));
    let ids = |client: &Client<SystemClock>| -> Vec<String> {
        client
            .tokens(soon())
            .unwrap()
            .into_iter()
            .map(|token| token.id.to_string())
            .collect()
    };
    assert_eq!(
        ids(&client),
        [
            phone.digest().id().to_string(),
            laptop.digest().id().to_string()
        ]
    );

    // The file keeps digests only, for this user alone, and lets the
    // tokens in.
    let path = scratch.0.join("data/tokens.json");
    let kept = fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&kept);
    assert!(!text.contains(phone.expose()) && !text.contains(&phone.expose()[4..]));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let book = TokenBook::parse(&kept).unwrap();
    assert!(book.check(&phone, SystemClock.wall()).is_ok());

    // Revoked, it's gone from the list and the file; revoking it again is
    // a wrong id.
    let revoked = client.revoke_token(&issued.id, soon()).unwrap();
    assert_eq!(revoked.id, issued.id);
    assert_eq!(ids(&client), [laptop.digest().id().to_string()]);
    let book = TokenBook::parse(&fs::read(&path).unwrap()).unwrap();
    assert!(book.check(&phone, SystemClock.wall()).is_err());
    match client.revoke_token(&issued.id, soon()) {
        Err(ClientError::Remote(RemoteError::InvalidArgument { field, .. })) => {
            assert_eq!(field, "id");
        }
        other => panic!("{other:?}"),
    }
}

/// A daemon connection that sends requests as written, even ones the
/// client's types can't express.
struct RawConnection {
    stream: std::os::unix::net::UnixStream,
    decoder: doppel_protocol::frame::FrameDecoder<doppel_protocol::pb::daemon::v1::DaemonFrame>,
}

impl RawConnection {
    fn open(socket: &std::path::Path) -> Self {
        Self::over(std::os::unix::net::UnixStream::connect(socket).unwrap())
    }

    fn over(stream: std::os::unix::net::UnixStream) -> Self {
        let mut connection = Self {
            stream,
            decoder: doppel_protocol::frame::FrameDecoder::new(
                doppel_protocol::frame::MaxFrameLen::DEFAULT,
            ),
        };
        connection.send(doppel_protocol::pb::daemon::v1::daemon_frame::Kind::Hello(
            doppel_protocol::pb::common::v1::Hello {
                protocol_version: doppel_protocol::PROTOCOL_VERSION,
                ..Default::default()
            },
        ));
        assert!(matches!(
            connection.receive(),
            doppel_protocol::pb::daemon::v1::daemon_frame::Kind::Hello(_)
        ));
        connection
    }

    fn send(&mut self, kind: doppel_protocol::pb::daemon::v1::daemon_frame::Kind) {
        use std::io::Write as _;
        let bytes =
            doppel_protocol::frame::FrameEncoder::new(doppel_protocol::frame::MaxFrameLen::DEFAULT)
                .encode(&doppel_protocol::pb::daemon::v1::DaemonFrame { kind: Some(kind) })
                .unwrap();
        self.stream.write_all(&bytes).unwrap();
    }

    fn receive(&mut self) -> doppel_protocol::pb::daemon::v1::daemon_frame::Kind {
        use std::io::Read as _;
        loop {
            if let Some(doppel_protocol::frame::Received::Recognized { message, .. }) =
                self.decoder.next_frame().unwrap()
            {
                return message.kind.unwrap();
            }
            let mut buffer = [0; 4096];
            let read = self.stream.read(&mut buffer).unwrap();
            assert!(read > 0, "the daemon hung up");
            self.decoder.push(&buffer[..read]);
        }
    }

    /// The outcome of one request.
    fn request(
        &mut self,
        operation: doppel_protocol::pb::daemon::v1::daemon_request::Operation,
    ) -> doppel_protocol::pb::daemon::v1::daemon_response::Outcome {
        use doppel_protocol::pb::daemon::v1 as daemon;
        self.send(daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
            why: String::new(),
            id: 1,
            caption: String::new(),
            operation: Some(operation),
        }));
        loop {
            if let daemon::daemon_frame::Kind::Response(response) = self.receive() {
                return response.outcome.unwrap();
            }
        }
    }
}

/// What the client's types can't send: a digest of the wrong length, a
/// lifetime or label out of range.
#[test]
fn token_requests_out_of_range_are_wrong_arguments() {
    use doppel_protocol::pb::common::v1 as common;
    use doppel_protocol::pb::daemon::v1 as daemon;

    let scratch = Scratch::new("tokbad");
    let socket = serve(&scratch);
    let mut connection = RawConnection::open(&socket);
    for (sha256, ttl_seconds, label, wrong) in [
        (vec![0; 31], 3_600, "", "sha256"),
        (vec![0; 32], 59, "", "ttl_seconds"),
        (vec![0; 32], 30 * 86_400 + 1, "", "ttl_seconds"),
        (vec![0; 32], 3_600, "a\nb", "label"),
    ] {
        let outcome = connection.request(daemon::daemon_request::Operation::IssueToken(
            daemon::IssueToken {
                sha256: sha256.into(),
                ttl_seconds,
                label: label.into(),
            },
        ));
        let daemon::daemon_response::Outcome::Error(common::Error {
            kind: Some(common::error::Kind::InvalidArgument(argument)),
            ..
        }) = &outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(argument.field, wrong);
    }
    assert!(!scratch.0.join("data/tokens.json").exists());
}

/// A browser's connection, bridged by the gateway, may do what any client
/// does except what only this Mac may: issue tokens, change the listener,
/// map the display's shared memory.
#[test]
fn a_remote_connection_is_refused_what_only_this_mac_may_ask() {
    use daemon::daemon_request::Operation;
    use daemon::daemon_response::Outcome;
    use doppel_gateway::{TokenBook, TokenSecret, Ttl};
    use doppel_protocol::pb::common::v1 as common;
    use doppel_protocol::pb::daemon::v1 as daemon;

    let scratch = Scratch::new("remote");
    scratch.machine("dev", &golden_spec());
    let daemon_ = Arc::new(scratch.daemon());
    let token = TokenBook::default()
        .add(
            TokenSecret::from_bytes([1; 32]).digest(),
            None,
            Ttl::DEFAULT,
            SystemClock.wall(),
        )
        .unwrap();
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let serving = {
        let daemon_ = Arc::clone(&daemon_);
        std::thread::spawn(move || daemon_.serve_remote(theirs, &token))
    };
    let mut browser = RawConnection::over(ours);
    for operation in [
        Operation::IssueToken(daemon::IssueToken {
            sha256: vec![0; 32].into(),
            ttl_seconds: 3_600,
            label: String::new(),
        }),
        Operation::ListTokens(daemon::ListTokens {}),
        Operation::RevokeToken(daemon::RevokeToken {
            id: "00000000".into(),
        }),
        Operation::ConfigureWeb(daemon::ConfigureWeb {
            listen: Some(daemon::configure_web::Listen::Off(daemon::Done {})),
        }),
        Operation::GetWebStatus(daemon::GetWebStatus {}),
        Operation::OpenDisplay(daemon::OpenDisplay { name: "dev".into() }),
    ] {
        let outcome = browser.request(operation.clone());
        assert!(
            matches!(
                &outcome,
                Outcome::Error(common::Error {
                    kind: Some(common::error::Kind::Unsupported(_)),
                })
            ),
            "{operation:?}: {outcome:?}"
        );
    }
    assert!(!scratch.0.join("data/tokens.json").exists());
    assert!(matches!(
        browser.request(Operation::ListMachines(daemon::ListMachines {})),
        Outcome::MachineList(list) if list.machines.len() == 1
    ));
    drop(browser);
    serving.join().unwrap();
}

/// The listener end to end: turned on, a token's holder reaches the daemon
/// through TLS and the WebSocket; revoked, the connection closes; turned
/// off, nothing answers.
#[test]
fn a_token_holder_reaches_the_daemon_through_the_listener() {
    use std::io::{Read as _, Write as _};

    use doppel_client::WebStatus;
    use doppel_gateway::{TokenSecret, Ttl};
    use doppel_protocol::pb::daemon::v1 as daemon;
    use prost::Message as _;
    use tungstenite::client::IntoClientRequest as _;

    let scratch = Scratch::new("web");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let client = Client::connect(&socket, SystemClock, soon()).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let WebStatus::Listening { names, .. } = client.configure_web(Some(port), soon()).unwrap()
    else {
        panic!("not listening");
    };
    assert!(names.iter().any(|name| name == "localhost"), "{names:?}");
    assert!(matches!(
        client.web_status(soon()).unwrap(),
        WebStatus::Listening { port: listening, .. } if listening == port
    ));
    let secret = TokenSecret::from_bytes([3; 32]);
    let token = client
        .issue_token(&secret.digest(), Ttl::DEFAULT, None, soon())
        .unwrap();

    // The certificate the daemon made, trusted as a browser would after a
    // person checked it.
    let cert = std::fs::read(scratch.0.join("data/tls/cert.pem")).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for der in
        <rustls::pki_types::CertificateDer as rustls::pki_types::pem::PemObject>::pem_slice_iter(
            &cert,
        )
    {
        roots.add(der.unwrap()).unwrap();
    }
    let tls = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth(),
    );
    let tcp = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let stream = rustls::StreamOwned::new(
        rustls::ClientConnection::new(tls, "localhost".try_into().unwrap()).unwrap(),
        tcp,
    );
    let mut request = format!("wss://localhost:{port}/socket")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Origin",
        format!("https://localhost:{port}").parse().unwrap(),
    );
    let (mut browser, _) = tungstenite::client::client(request, stream).unwrap();
    browser
        .send(tungstenite::Message::text(secret.expose()))
        .unwrap();
    let mut send = |kind: daemon::daemon_frame::Kind| {
        browser
            .send(tungstenite::Message::binary(
                daemon::DaemonFrame { kind: Some(kind) }.encode_to_vec(),
            ))
            .unwrap();
    };
    send(daemon::daemon_frame::Kind::Hello(
        doppel_protocol::pb::common::v1::Hello {
            protocol_version: doppel_protocol::PROTOCOL_VERSION,
            ..Default::default()
        },
    ));
    send(daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
        why: String::new(),
        id: 1,
        caption: String::new(),
        operation: Some(daemon::daemon_request::Operation::ListMachines(
            daemon::ListMachines {},
        )),
    }));
    let mut frames = Vec::new();
    while frames.len() < 2 {
        let message = browser.read().unwrap();
        let tungstenite::Message::Binary(bytes) = message else {
            panic!("{message:?}");
        };
        frames.push(daemon::DaemonFrame::decode(bytes).unwrap().kind.unwrap());
    }
    assert!(matches!(frames[0], daemon::daemon_frame::Kind::Hello(_)));
    let daemon::daemon_frame::Kind::Response(response) = &frames[1] else {
        panic!("{:?}", frames[1]);
    };
    assert!(matches!(
        &response.outcome,
        Some(daemon::daemon_response::Outcome::MachineList(list)) if list.machines.len() == 1
    ));

    // Revoked: closed at once.
    client.revoke_token(&token.id, soon()).unwrap();
    let closed = browser.read().unwrap();
    let tungstenite::Message::Close(Some(frame)) = &closed else {
        panic!("{closed:?}");
    };
    assert_eq!(
        frame.code,
        tungstenite::protocol::frame::coding::CloseCode::Policy
    );

    // Off: the port no longer answers.
    assert_eq!(client.configure_web(None, soon()).unwrap(), WebStatus::Off);
    let refused = std::net::TcpStream::connect(("127.0.0.1", port)).and_then(|mut tcp| {
        tcp.set_read_timeout(Some(Duration::from_secs(2)))?;
        tcp.write_all(b"x")?;
        let mut byte = [0];
        tcp.read(&mut byte)
    });
    assert!(matches!(refused, Err(_) | Ok(0)), "{refused:?}");
}

#[test]
fn a_stopped_machine_is_deleted_whole_and_only_once() {
    use daemon::daemon_request::Operation;
    use daemon::daemon_response::Outcome;
    use doppel_protocol::pb::daemon::v1 as daemon;

    let scratch = Scratch::new("delete");
    scratch.machine("dev", &golden_spec());
    scratch.machine("keep", &golden_spec());
    let socket = serve(&scratch);
    let dev = scratch.store().bundle(&"dev".parse().unwrap());
    fs::write(dev.dir().join("disk.img"), b"a disk").unwrap();
    let mut connection = RawConnection::open(&socket);
    let delete = |connection: &mut RawConnection, name: &str| {
        connection.request(Operation::DeleteMachine(daemon::DeleteMachine {
            name: name.into(),
        }))
    };

    let outcome = delete(&mut connection, "dev");
    assert!(matches!(outcome, Outcome::Done(_)), "{outcome:?}");
    assert!(!dev.dir().exists(), "its folder is gone");
    let Outcome::MachineList(list) =
        connection.request(Operation::ListMachines(daemon::ListMachines {}))
    else {
        panic!("no list");
    };
    let names: Vec<&str> = list.machines.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["keep"]);
    assert!(list.unrecognized.is_empty(), "{:?}", list.unrecognized);
    // Nothing left in `deleting/` either.
    let deleting = scratch.store().root().path().join("deleting");
    assert_eq!(fs::read_dir(&deleting).unwrap().count(), 0);

    // Again: there's nothing to delete.
    let again = delete(&mut connection, "dev");
    let Outcome::MachineFailure(failure) = &again else {
        panic!("{again:?}");
    };
    assert_eq!(
        failure.certainty,
        daemon::Certainty::NotDone as i32,
        "{failure:?}"
    );
}

#[test]
fn a_watcher_sees_an_agent_s_calls_start_and_end_and_no_one_else_s() {
    use daemon::daemon_request::Operation;
    use daemon::daemon_response::Outcome;
    use doppel_protocol::pb::common::v1 as common;
    use doppel_protocol::pb::daemon::v1 as daemon;

    let scratch = Scratch::new("watch");
    scratch.machine("dev", &golden_spec());
    let socket = serve(&scratch);
    let mut watcher = RawConnection::open(&socket);
    watcher
        .stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let watching = watcher.request(Operation::WatchActivity(daemon::WatchActivity {}));
    assert!(matches!(watching, Outcome::Done(_)), "{watching:?}");
    let click = || {
        Operation::SendInput(daemon::SendInput {
            name: "dev".into(),
            via: daemon::Via::Host.into(),
            input: Some(common::Input {
                action: Some(common::input::Action::Click(common::Click {
                    at: Some(common::Point { x: 12.0, y: 34.0 }),
                    button: common::MouseButton::Left.into(),
                    count: 1,
                })),
            }),
        })
    };

    // Outside a session, a call isn't shown.
    let mut person = RawConnection::open(&socket);
    person.request(click());
    let mut agent = RawConnection::open(&socket);
    let opened = agent.request(Operation::OpenSession(daemon::OpenSession {
        client: "test agent".into(),
        record: false,
    }));
    let Outcome::SessionOpened(opened) = opened else {
        panic!("{opened:?}");
    };
    agent.send(daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
        why: String::new(),
        id: 9,
        caption: "click Save".into(),
        operation: Some(click()),
    }));

    let mut next = || loop {
        if let daemon::daemon_frame::Kind::Event(daemon::DaemonEvent {
            kind: Some(daemon::daemon_event::Kind::Activity(activity)),
            ..
        }) = watcher.receive()
        {
            return activity;
        }
    };
    let started = next();
    assert_eq!(
        (started.session, started.request, started.machine.as_str()),
        (opened.session, 9, "dev")
    );
    assert_eq!(started.client, "test agent");
    assert_eq!(started.caption, "click Save");
    let Some(daemon::activity::Phase::Started(call)) = &started.phase else {
        panic!("{started:?}");
    };
    assert_eq!(call.gesture(), daemon::Gesture::Click);
    assert_eq!(
        call.target,
        Some(daemon::call_started::Target::Point(common::Point {
            x: 12.0,
            y: 34.0
        }))
    );
    let ended = next();
    assert_eq!((ended.session, ended.request), (opened.session, 9));
    // The machine isn't running: the click failed, and says so.
    let Some(daemon::activity::Phase::Ended(daemon::CallEnded {
        outcome: Some(daemon::call_ended::Outcome::Failed(_)),
        seen: None,
        refs: None,
        read: None,
    })) = &ended.phase
    else {
        panic!("{ended:?}");
    };
}

#[test]
fn a_stopped_daemon_closes_its_connections_and_returns() {
    let scratch = Scratch::new("stop");
    let daemon = Arc::new(scratch.daemon());
    let listener = daemon.listen().unwrap();
    let socket = listener.path().to_path_buf();
    let stopper = daemon.stopper();
    let (returned, served) = std::sync::mpsc::channel();
    std::thread::spawn({
        let daemon = Arc::clone(&daemon);
        move || returned.send(daemon.serve(&listener).is_ok())
    });
    let agent = Client::connect(&socket, SystemClock, soon()).unwrap();
    agent
        .open_session(
            "doppel mcp (test)",
            doppel_client::Record::IntoStore,
            soon(),
        )
        .unwrap();
    stopper.stop();
    assert_eq!(served.recv_timeout(Duration::from_secs(5)), Ok(true));
    assert!(agent.sessions(soon()).is_err(), "its connection closed");
    assert!(Client::connect(&socket, SystemClock, soon()).is_err());
}

#[test]
fn a_daemon_stopped_before_it_serves_returns_at_once() {
    let scratch = Scratch::new("stop0");
    let daemon = scratch.daemon();
    let listener = daemon.listen().unwrap();
    daemon.stopper().stop();
    daemon.serve(&listener).unwrap();
}

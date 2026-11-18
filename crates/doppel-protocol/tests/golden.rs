//! Cross-language contract: these streams are committed under `fixtures/wire/`.
//! The Swift tests (`apple/Tests/DoppelWireTests`) decode the same files, check
//! the same values and re-encode them byte-for-byte.
//!
//! Regenerate after an intentional schema change:
//! `DOPPEL_UPDATE_GOLDEN=1 cargo test -p doppel-protocol --test golden`

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::path::PathBuf;

use doppel_protocol::exchange::Protocol;
use doppel_protocol::frame::{Envelope, FrameDecoder, FrameEncoder, MaxFrameLen, Received};
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::vmm::v1 as vmm;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/wire")
        .join(name)
}

fn point(x: f64, y: f64) -> Option<common::Point> {
    Some(common::Point { x, y })
}

fn agent_stream() -> Vec<agent::AgentFrame> {
    use agent::agent_frame::Kind;
    use agent::agent_request::Operation;
    use agent::agent_response::Outcome;
    vec![
        agent::AgentFrame {
            kind: Some(Kind::Hello(common::Hello {
                protocol_version: agent::AgentFrame::VERSION,
                software_version: "doppel-agent 0.1.0".into(),
                build_id: String::new(),
                capabilities: vec![
                    common::Capability::Screenshot as i32,
                    common::Capability::Input as i32,
                    common::Capability::AxSnapshot as i32,
                    common::Capability::AxObserve as i32,
                ],
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 1,
                operation: Some(Operation::Input(common::Input {
                    action: Some(common::input::Action::Click(common::Click {
                        at: point(640.5, 400.0),
                        button: common::MouseButton::Left as i32,
                        count: 2,
                    })),
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 2,
                operation: Some(Operation::Input(common::Input {
                    action: Some(common::input::Action::Key(common::KeyChord {
                        modifiers: vec![
                            common::Modifier::Command as i32,
                            common::Modifier::Shift as i32,
                        ],
                        key: Some(common::Key {
                            key: Some(common::key::Key::Character("é".into())),
                        }),
                    })),
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 3,
                operation: Some(Operation::TakeAxSnapshot(agent::TakeAxSnapshot {
                    scope: Some(agent::take_ax_snapshot::Scope::FocusedWindow(
                        agent::Empty {},
                    )),
                    max_nodes: 2000,
                    max_depth: 64,
                    deadline_ms: 500,
                    menus: agent::MenuPolicy::Exclude as i32,
                    retention: agent::Retention::Observation as i32,
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Response(agent::AgentResponse {
                id: 3,
                outcome: Some(Outcome::AxSnapshot(agent::AxSnapshot {
                    snapshot_id: 42,
                    app: Some(agent::RunningApp {
                        pid: 501,
                        bundle_id: "com.apple.finder".into(),
                        name: "Finder".into(),
                    }),
                    nodes: vec![
                        agent::AxNode {
                            parent: None,
                            role: "AXWindow".into(),
                            title: "Downloads".into(),
                            frame: Some(common::Rect {
                                origin: point(0.0, 25.0),
                                size: Some(common::Size {
                                    width: 800.0,
                                    height: 600.0,
                                }),
                            }),
                            flags: Some(agent::AxFlags {
                                focused: Some(true),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        agent::AxNode {
                            parent: Some(0),
                            role: "AXTextField".into(),
                            subrole: "AXSearchField".into(),
                            value: Some(agent::AxValue {
                                value: Some(agent::ax_value::Value::Text(String::new())),
                            }),
                            flags: Some(agent::AxFlags {
                                enabled: Some(false),
                                ..Default::default()
                            }),
                            actions: vec!["AXPress".into(), "AXConfirm".into()],
                            ..Default::default()
                        },
                    ],
                    truncation: agent::Truncation::Complete as i32,
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Response(agent::AgentResponse {
                id: 4,
                outcome: Some(Outcome::Error(common::Error {
                    kind: Some(common::error::Kind::StaleReference(
                        common::StaleReference {
                            snapshot_id: 41,
                            node_index: 7,
                        },
                    )),
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Response(agent::AgentResponse {
                id: 5,
                outcome: Some(Outcome::ExecResult(agent::ExecResult {
                    termination: Some(agent::exec_result::Termination::ExitCode(0)),
                    stdout: b"hello\n".to_vec().into(),
                    stderr: Default::default(),
                    completeness: agent::OutputCompleteness::Full as i32,
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Event(agent::AgentEvent {
                kind: Some(agent::agent_event::Kind::PermissionsChanged(
                    agent::PermissionsChanged {
                        granted: vec![common::Permission::Accessibility as i32],
                    },
                )),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 6,
                operation: Some(Operation::Subscribe(agent::Subscribe {})),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Event(agent::AgentEvent {
                kind: Some(agent::agent_event::Kind::FrontmostAppChanged(
                    agent::FrontmostAppChanged {
                        app: Some(agent::RunningApp {
                            pid: 612,
                            bundle_id: "com.apple.TextEdit".into(),
                            name: "TextEdit".into(),
                        }),
                    },
                )),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Event(agent::AgentEvent {
                kind: Some(agent::agent_event::Kind::UiChanged(agent::UiChanged {
                    pid: 612,
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 7,
                operation: Some(Operation::ShowProbe(agent::ShowProbe {
                    frame: Some(common::Rect {
                        origin: point(0.0, 0.0),
                        size: Some(common::Size {
                            width: 4.0,
                            height: 4.0,
                        }),
                    }),
                    color: Some(common::Color {
                        red: 0,
                        green: 0,
                        blue: 255,
                    }),
                })),
            })),
        },
        agent::AgentFrame {
            kind: Some(Kind::Request(agent::AgentRequest {
                id: 8,
                operation: Some(Operation::HideProbe(agent::HideProbe {})),
            })),
        },
    ]
}

fn vmm_stream() -> Vec<vmm::VmmFrame> {
    use vmm::vmm_frame::Kind;
    vec![
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 9,
                operation: Some(vmm::vmm_request::Operation::SaveState(vmm::SaveState {
                    path: "/tmp/checkpoints/before-install/memory.vzsave".into(),
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 9,
                outcome: Some(vmm::vmm_response::Outcome::StateSaved(vmm::StateSaved {
                    bytes: 1_744_830_464,
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::StateChanged(vmm::VmState {
                    run_state: vmm::VmRunState::Paused as i32,
                    window: vmm::WindowMode::Unspecified as i32,
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::InstallProgress(
                    vmm::InstallProgress { fraction: 0.25 },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 10,
                operation: Some(vmm::vmm_request::Operation::OpenDisplay(
                    vmm::OpenDisplay {},
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 10,
                outcome: Some(vmm::vmm_response::Outcome::DisplayOpened(
                    vmm::DisplayOpened {
                        layout: Some(vmm::DisplayLayout {
                            width_pixels: 2560,
                            height_pixels: 1600,
                            display_points: Some(common::Size {
                                width: 1280.0,
                                height: 800.0,
                            }),
                            slot_count: 3,
                            slot_stride: 6_160_384,
                            luma_offset: 64,
                            luma_bytes_per_row: 2560,
                            chroma_offset: 4_096_064,
                            chroma_bytes_per_row: 2560,
                        }),
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::DisplayFrame(vmm::DisplayFrame {
                    slot: 2,
                    sequence: 41,
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 11,
                operation: Some(vmm::vmm_request::Operation::HostInput(common::Input {
                    action: Some(common::input::Action::Pointer(common::Pointer {
                        phase: common::PointerPhase::Move as i32,
                        at: point(640.5, 400.0),
                        button: common::MouseButton::Left as i32,
                        click_count: 0,
                        modifiers: vec![common::Modifier::Shift as i32],
                    })),
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 12,
                operation: Some(vmm::vmm_request::Operation::StopRecording(
                    vmm::StopRecording {
                        path: Some("/tmp/runs/dev-001.mp4".into()),
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 12,
                outcome: Some(vmm::vmm_response::Outcome::RecordingStopped(
                    vmm::RecordingStopped {
                        path: "/tmp/runs/dev-001.mp4".into(),
                        frames: 1_812,
                        seconds: 95.5,
                        bytes: 48_000_000,
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 13,
                operation: Some(vmm::vmm_request::Operation::OpenEncodedDisplay(
                    vmm::OpenEncodedDisplay {},
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 13,
                outcome: Some(vmm::vmm_response::Outcome::EncodedDisplayOpened(
                    vmm::EncodedDisplayOpened {
                        width_pixels: 2560,
                        height_pixels: 1600,
                        display_points: Some(common::Size {
                            width: 1280.0,
                            height: 800.0,
                        }),
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::EncodedFrame(vmm::EncodedFrame {
                    sequence: 1,
                    key: true,
                    parameter_sets: vec![
                        vec![0x67, 0x64, 0x00, 0x28].into(),
                        vec![0x68, 0xee].into(),
                    ],
                    data: vec![0, 0, 0, 3, 0x65, 0x88, 0x84].into(),
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 14,
                outcome: Some(vmm::vmm_response::Outcome::State(vmm::VmState {
                    run_state: vmm::VmRunState::Running as i32,
                    window: vmm::WindowMode::Shown as i32,
                })),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Request(vmm::VmmRequest {
                id: 15,
                operation: Some(vmm::vmm_request::Operation::SampleDisplay(
                    vmm::SampleDisplay {
                        at: point(2.0, 2.0),
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Response(vmm::VmmResponse {
                id: 15,
                outcome: Some(vmm::vmm_response::Outcome::DisplaySample(
                    vmm::DisplaySample {
                        color: Some(common::Color {
                            red: 236,
                            green: 237,
                            blue: 238,
                        }),
                    },
                )),
            })),
        },
        vmm::VmmFrame {
            kind: Some(Kind::Hello(common::Hello {
                protocol_version: vmm::VmmFrame::VERSION,
                software_version: "doppel-vmm 0.1.0".into(),
                build_id: String::new(),
                capabilities: Vec::new(),
            })),
        },
    ]
}

/// Rust only: no Swift process speaks to the daemon.
/// A golden image installed from a restore image, its template `ready`.
fn golden_lineage() -> daemon::Lineage {
    daemon::Lineage {
        role: Some(daemon::lineage::Role::Golden(daemon::GoldenRole {
            checkpoint: "ready".into(),
            agent_build: "20261118T164320Z-7da947b".into(),
        })),
        origin: Some(daemon::lineage::Origin::Installed(daemon::Done {})),
    }
}

/// A working machine forked from `golden`'s `ready` checkpoint.
fn forked_lineage() -> daemon::Lineage {
    daemon::Lineage {
        role: Some(daemon::lineage::Role::Working(daemon::Done {})),
        origin: Some(daemon::lineage::Origin::Forked(daemon::ForkedFrom {
            machine: "golden".into(),
            checkpoint: "ready".into(),
        })),
    }
}

fn daemon_stream() -> Vec<daemon::DaemonFrame> {
    use daemon::daemon_frame::Kind;
    use daemon::daemon_request::Operation;
    use daemon::daemon_response::Outcome;
    vec![
        daemon::DaemonFrame {
            kind: Some(Kind::Hello(common::Hello {
                protocol_version: daemon::DaemonFrame::VERSION,
                software_version: "doppel 0.1.0".into(),
                build_id: String::new(),
                capabilities: Vec::new(),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                caption: String::new(),
                id: 1,
                operation: Some(Operation::ListMachines(daemon::ListMachines {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 1,
                handed_back: None,
                outcome: Some(Outcome::MachineList(daemon::MachineList {
                    machines: vec![
                        daemon::MachineEntry {
                            name: "dev".into(),
                            status: Some(daemon::machine_entry::Status::MachineStatus(
                                daemon::MachineStatus {
                                    kind: Some(daemon::machine_status::Kind::Live(vmm::VmState {
                                        run_state: vmm::VmRunState::Running as i32,
                                        window: vmm::WindowMode::Unspecified as i32,
                                    })),
                                },
                            )),
                            recorded_lineage: Some(
                                daemon::machine_entry::RecordedLineage::Lineage(forked_lineage()),
                            ),
                        },
                        daemon::MachineEntry {
                            name: "golden".into(),
                            status: Some(daemon::machine_entry::Status::MachineStatus(
                                daemon::MachineStatus {
                                    kind: Some(daemon::machine_status::Kind::Stopped(
                                        daemon::NextStart::ResumeSavedState as i32,
                                    )),
                                },
                            )),
                            recorded_lineage: Some(
                                daemon::machine_entry::RecordedLineage::Lineage(golden_lineage()),
                            ),
                        },
                        daemon::MachineEntry {
                            name: "broken".into(),
                            status: Some(daemon::machine_entry::Status::Failure(
                                daemon::MachineFailure {
                                    message: "broken: spec.pb is invalid".into(),
                                    certainty: daemon::Certainty::NotDone as i32,
                                    causes: Vec::new(),
                                },
                            )),
                            recorded_lineage: Some(
                                daemon::machine_entry::RecordedLineage::LineageUnreadable(
                                    "lineage.pb: missing field `lineage.role`".into(),
                                ),
                            ),
                        },
                        daemon::MachineEntry {
                            name: "old".into(),
                            status: Some(daemon::machine_entry::Status::MachineStatus(
                                daemon::MachineStatus {
                                    kind: Some(daemon::machine_status::Kind::Stopped(
                                        daemon::NextStart::ColdBoot as i32,
                                    )),
                                },
                            )),
                            recorded_lineage: Some(
                                daemon::machine_entry::RecordedLineage::Lineage(daemon::Lineage {
                                    role: Some(daemon::lineage::Role::Working(daemon::Done {})),
                                    origin: Some(daemon::lineage::Origin::Unrecorded(
                                        daemon::Done {},
                                    )),
                                }),
                            ),
                        },
                    ],
                    unrecognized: vec![b"Not A Name".to_vec().into()],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                caption: String::new(),
                id: 2,
                operation: Some(Operation::GetStatus(daemon::GetStatus {
                    name: "nope".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 2,
                handed_back: None,
                outcome: Some(Outcome::MachineFailure(daemon::MachineFailure {
                    message: "no machine named nope".into(),
                    certainty: daemon::Certainty::NotDone as i32,
                    causes: Vec::new(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                caption: String::new(),
                id: 3,
                operation: Some(Operation::StartMachine(daemon::StartMachine {
                    name: "dev".into(),
                    window: daemon::Window::Hidden as i32,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 3,
                handed_back: None,
                outcome: Some(Outcome::MachineStarted(daemon::MachineStarted {
                    state: Some(vmm::VmState {
                        run_state: vmm::VmRunState::Running as i32,
                        window: vmm::WindowMode::Unspecified as i32,
                    }),
                    started_by: daemon::NextStart::ResumeSavedState as i32,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 4,
                handed_back: None,
                outcome: Some(Outcome::LoginResult(daemon::LoginResult {
                    outcome: Some(daemon::login_result::Outcome::UnlockedAfterMicros(
                        1_119_000,
                    )),
                    screen: Some(daemon::HostScreen {
                        state: Some(daemon::host_screen::State::CurrentAfterMicros(3_480_000)),
                    }),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                caption: String::new(),
                id: 5,
                operation: Some(Operation::TakeCheckpoint(daemon::TakeCheckpoint {
                    name: "dev".into(),
                    label: "clean".into(),
                    then: daemon::AfterCheckpoint::Resume as i32,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 5,
                handed_back: None,
                outcome: Some(Outcome::CheckpointReport(daemon::CheckpointReport {
                    result: Some(daemon::checkpoint_report::Result::Failed(
                        daemon::CheckpointFailed {
                            at: daemon::CheckpointStep::SavingMemory as i32,
                            error: Some(daemon::MachineFailure {
                                message: "timed out waiting for the VMM".into(),
                                certainty: daemon::Certainty::MaybeDone as i32,
                                causes: vec!["no answer in 300 s".into()],
                            }),
                            guest: daemon::GuestAfterFailure::ResumeRequested as i32,
                        },
                    )),
                    paused_for_micros: Some(4_700_000),
                    cleanup: vec![daemon::CheckpointCleanup {
                        action: "Resume".into(),
                        failure: None,
                    }],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 6,
                handed_back: None,
                outcome: Some(Outcome::MachineStopped(daemon::MachineStopped {
                    request: Some(daemon::machine_stopped::Request::PowerButton(
                        "the screen is locked: no password is stored".into(),
                    )),
                    outcome: daemon::StopOutcome::ShutdownPending as i32,
                })),
            })),
        },
        // A session: the connection's requests from here on are its own.
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 7,
                caption: String::new(),
                operation: Some(Operation::OpenSession(daemon::OpenSession {
                    client: "doppel mcp (claude-code)".into(),
                    record: true,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 7,
                handed_back: None,
                outcome: Some(Outcome::SessionOpened(daemon::SessionOpened {
                    session: 3,
                    recording: Some(
                        b"/Users/someone/Doppel/recordings/2026-11-13T21-04-05Z-s3"
                            .to_vec()
                            .into(),
                    ),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 8,
                caption: "press s2e5 (button \"Save\")".into(),
                operation: Some(Operation::ActOnElement(daemon::ActOnElement {
                    name: "dev".into(),
                    action: Some(agent::PerformAxAction {
                        snapshot_id: 2,
                        node_index: 5,
                        action: Some(agent::perform_ax_action::Action::Press(agent::Empty {})),
                    }),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 9,
                caption: "wait_for".into(),
                operation: Some(Operation::WaitForElement(daemon::WaitForElement {
                    name: "dev".into(),
                    condition: Some(daemon::wait_for_element::Condition::Disappears(
                        "Save".into(),
                    )),
                    timeout_ms: 5000,
                    spec: Some(daemon::SnapshotSpec {
                        scope: daemon::SnapshotScope::FocusedWindow as i32,
                        menus: agent::MenuPolicy::Exclude as i32,
                    }),
                })),
            })),
        },
        // Answered out of order: the wait was cancelled before the press ended.
        daemon::DaemonFrame {
            kind: Some(Kind::Cancel(daemon::CancelRequest { id: 9 })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 9,
                handed_back: None,
                outcome: Some(Outcome::MachineFailure(daemon::MachineFailure {
                    message: "cancelled".into(),
                    certainty: daemon::Certainty::NotDone as i32,
                    causes: Vec::new(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 8,
                handed_back: None,
                outcome: Some(Outcome::ActResult(daemon::ActResult {
                    outcome: daemon::ActOutcome::StaleReference as i32,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 10,
                handed_back: None,
                outcome: Some(Outcome::ElementWait(daemon::ElementWaitResult {
                    end: Some(daemon::element_wait_result::End::TimedOut(
                        daemon::WaitTimedOut {
                            last_error: Some("no window yet".into()),
                            elapsed_micros: 5_012_000,
                            polls: 7,
                        },
                    )),
                    seen: None,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 11,
                handed_back: None,
                outcome: Some(Outcome::SessionList(daemon::SessionList {
                    sessions: vec![daemon::SessionEntry {
                        id: 3,
                        client: "doppel mcp (claude-code)".into(),
                        machines: vec!["dev".into()],
                        answered: 41,
                        running: vec!["wait_for".into()],
                        // Left unset, as before sessions had states.
                        state: daemon::SessionState::Unspecified as i32,
                    }],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 12,
                caption: String::new(),
                operation: Some(Operation::OpenDisplay(daemon::OpenDisplay {
                    name: "dev".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 12,
                handed_back: None,
                outcome: Some(Outcome::DisplayLayout(vmm::DisplayLayout {
                    width_pixels: 2560,
                    height_pixels: 1600,
                    display_points: Some(common::Size {
                        width: 1280.0,
                        height: 800.0,
                    }),
                    slot_count: 3,
                    slot_stride: 6_160_384,
                    luma_offset: 64,
                    luma_bytes_per_row: 2560,
                    chroma_offset: 4_096_064,
                    chroma_bytes_per_row: 2560,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 12,
                kind: Some(daemon::daemon_event::Kind::DisplayFrame(
                    vmm::DisplayFrame {
                        slot: 2,
                        sequence: 1_207,
                    },
                )),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 13,
                caption: String::new(),
                operation: Some(Operation::SendHostInput(daemon::SendHostInput {
                    name: "dev".into(),
                    input: Some(common::Input {
                        action: Some(common::input::Action::Pointer(common::Pointer {
                            phase: common::PointerPhase::Press as i32,
                            at: point(640.5, 400.0),
                            button: common::MouseButton::Left as i32,
                            click_count: 1,
                            modifiers: Vec::new(),
                        })),
                    }),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 14,
                caption: String::new(),
                operation: Some(Operation::TakeControl(daemon::TakeControl {
                    name: "dev".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 15,
                caption: String::new(),
                operation: Some(Operation::HandBack(daemon::HandBack { name: "dev".into() })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 16,
                caption: String::new(),
                operation: Some(Operation::PauseSession(daemon::PauseSession { session: 3 })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 17,
                caption: String::new(),
                operation: Some(Operation::ResumeSession(daemon::ResumeSession {
                    session: 3,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 18,
                handed_back: Some(daemon::HandedBack {
                    machine: "dev".into(),
                    held_millis: 42_000,
                }),
                outcome: Some(Outcome::Done(daemon::Done {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 19,
                handed_back: None,
                outcome: Some(Outcome::SessionList(daemon::SessionList {
                    sessions: vec![daemon::SessionEntry {
                        id: 3,
                        client: "doppel mcp (claude-code)".into(),
                        machines: vec!["dev".into()],
                        answered: 41,
                        running: vec!["snapshot".into()],
                        state: daemon::SessionState::Paused as i32,
                    }],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 20,
                caption: String::new(),
                operation: Some(Operation::OpenEncodedDisplay(daemon::OpenEncodedDisplay {
                    name: "dev".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 20,
                handed_back: None,
                outcome: Some(Outcome::EncodedDisplay(vmm::EncodedDisplayOpened {
                    width_pixels: 2560,
                    height_pixels: 1600,
                    display_points: Some(common::Size {
                        width: 1280.0,
                        height: 800.0,
                    }),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 20,
                kind: Some(daemon::daemon_event::Kind::EncodedFrame(
                    vmm::EncodedFrame {
                        sequence: 2,
                        key: false,
                        parameter_sets: Vec::new(),
                        data: vec![0, 0, 0, 2, 0x41, 0x9a].into(),
                    },
                )),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 21,
                caption: String::new(),
                operation: Some(Operation::RunForks(daemon::RunForks {
                    name: "dev".into(),
                    label: "clean".into(),
                    count: 2,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 21,
                handed_back: None,
                outcome: Some(Outcome::ForksRun(daemon::ForksRun {
                    forks: vec![
                        daemon::RunFork {
                            name: "dev-clean-1".into(),
                            outcome: Some(daemon::run_fork::Outcome::Desktop(
                                daemon::LoginResult {
                                    outcome: Some(
                                        daemon::login_result::Outcome::UnlockedAfterMicros(
                                            21_400_000,
                                        ),
                                    ),
                                    screen: Some(daemon::HostScreen {
                                        state: Some(daemon::host_screen::State::ProbeUnsupported(
                                            daemon::Done {},
                                        )),
                                    }),
                                },
                            )),
                        },
                        daemon::RunFork {
                            name: "dev-clean-2".into(),
                            outcome: Some(daemon::run_fork::Outcome::Failure(
                                daemon::MachineFailure {
                                    message: "the VMM exited before its socket appeared".into(),
                                    certainty: daemon::Certainty::NotDone as i32,
                                    causes: Vec::new(),
                                },
                            )),
                        },
                    ],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 22,
                caption: String::new(),
                operation: Some(Operation::IssueToken(daemon::IssueToken {
                    sha256: vec![0xab; 32].into(),
                    ttl_seconds: 43_200,
                    label: "phone".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 22,
                handed_back: None,
                outcome: Some(Outcome::Token(daemon::TokenEntry {
                    id: "abababab".into(),
                    label: "phone".into(),
                    issued_unix_seconds: 1_794_603_845,
                    expires_unix_seconds: 1_794_647_045,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 23,
                caption: String::new(),
                operation: Some(Operation::ListTokens(daemon::ListTokens {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 23,
                handed_back: None,
                outcome: Some(Outcome::TokenList(daemon::TokenList {
                    tokens: vec![daemon::TokenEntry {
                        id: "abababab".into(),
                        label: String::new(),
                        issued_unix_seconds: 1_794_603_845,
                        expires_unix_seconds: 1_794_647_045,
                    }],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 24,
                caption: String::new(),
                operation: Some(Operation::RevokeToken(daemon::RevokeToken {
                    id: "abababab".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 25,
                caption: String::new(),
                operation: Some(Operation::ConfigureWeb(daemon::ConfigureWeb {
                    listen: Some(daemon::configure_web::Listen::On(daemon::WebPort {
                        port: 7443,
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 25,
                handed_back: None,
                outcome: Some(Outcome::WebStatus(daemon::WebStatus {
                    state: Some(daemon::web_status::State::Listening(daemon::WebListening {
                        port: 7443,
                        fingerprint: "AB:CD".into(),
                        names: vec!["mac.local".into(), "localhost".into()],
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 26,
                caption: String::new(),
                operation: Some(Operation::GetWebStatus(daemon::GetWebStatus {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 26,
                handed_back: None,
                outcome: Some(Outcome::WebStatus(daemon::WebStatus {
                    state: Some(daemon::web_status::State::Failed(daemon::WebFailed {
                        port: 80,
                        reason: "Permission denied".into(),
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 27,
                caption: String::new(),
                operation: Some(Operation::ListRecordings(daemon::ListRecordings {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 27,
                handed_back: None,
                outcome: Some(Outcome::RecordingList(daemon::RecordingList {
                    recordings: vec![daemon::RecordingSummary {
                        id: "2026-11-14T06-15-42Z-s1".into(),
                        started_unix_seconds: 1_794_636_942,
                        session: 1,
                        machines: vec!["dev".into()],
                        videos: 2,
                        calls: 3,
                        traced_ms: 9_500,
                    }],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 28,
                caption: String::new(),
                operation: Some(Operation::OpenRecording(daemon::OpenRecording {
                    id: "2026-11-14T06-15-42Z-s1".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 32,
                caption: String::new(),
                operation: Some(Operation::OpenSeenSnapshot(daemon::OpenSeenSnapshot {
                    recording: "2026-11-14T09-01-02Z-s2".into(),
                    line: 0,
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 34,
                caption: String::new(),
                operation: Some(Operation::ObserveElements(daemon::ObserveElements {
                    name: "live".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 35,
                caption: String::new(),
                operation: Some(Operation::ObserveText(daemon::ObserveText {
                    name: "live".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 36,
                caption: "mark s3e12".into(),
                operation: Some(Operation::MarkScreen(daemon::MarkScreen {
                    name: "live".into(),
                    shape: daemon::MarkShape::Arrow.into(),
                    at: Some(daemon::mark_screen::At::Element(daemon::ElementOf {
                        snapshot_id: 900,
                        node_index: 4,
                    })),
                    from: Some(common::Point { x: 10.0, y: 20.0 }),
                    note: "the total".into(),
                    lasts: Some(daemon::mark_screen::Lasts::UntilActedMs(5000)),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 33,
                caption: String::new(),
                operation: Some(Operation::NameElements(daemon::NameElements {
                    machine: "dev".into(),
                    refs: Some(daemon::ElementRefs {
                        snapshot: 3,
                        generation: 3,
                        nodes: vec![1],
                    }),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 28,
                handed_back: None,
                outcome: Some(Outcome::Recording(daemon::Recording {
                    id: "2026-11-14T06-15-42Z-s1".into(),
                    videos: vec![
                        daemon::RecordingVideo {
                            file: "dev-001.mp4".into(),
                            machine: "dev".into(),
                            start_ms: Some(98),
                            display_points: None,
                        },
                        daemon::RecordingVideo {
                            file: "dev-002.mp4".into(),
                            machine: "dev".into(),
                            start_ms: None,
                            display_points: None,
                        },
                    ],
                    trace: vec![
                        daemon::TraceLine {
                            line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                                start_ms: 99,
                                end_ms: 122,
                                machine: Some("dev".into()),
                                call: "status".into(),
                                outcome: daemon::TraceOutcome::Done as i32,
                                error: None,
                                waited_ms: None,
                                by: daemon::TraceActor::Session as i32,
                                acted: None,
                                queued_ms: None,
                                seen: false,
                                refs: None,
                                read: None,
                            })),
                        },
                        daemon::TraceLine {
                            line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                                start_ms: 2_000,
                                end_ms: 7_800,
                                machine: Some("dev".into()),
                                call: "a person had control".into(),
                                outcome: daemon::TraceOutcome::Done as i32,
                                error: None,
                                waited_ms: None,
                                by: daemon::TraceActor::Person as i32,
                                acted: None,
                                queued_ms: None,
                                seen: false,
                                refs: None,
                                read: None,
                            })),
                        },
                        daemon::TraceLine {
                            line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                                start_ms: 7_900,
                                end_ms: 9_500,
                                machine: None,
                                call: "wait_for".into(),
                                outcome: daemon::TraceOutcome::TimedOut as i32,
                                error: Some("no VMM".into()),
                                waited_ms: Some(5_700),
                                by: daemon::TraceActor::Session as i32,
                                acted: None,
                                queued_ms: Some(3_324),
                                seen: false,
                                refs: None,
                                read: None,
                            })),
                        },
                        daemon::TraceLine {
                            line: Some(daemon::trace_line::Line::Unreadable(
                                "{\"start_ms\":1".into(),
                            )),
                        },
                    ],
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 29,
                caption: String::new(),
                operation: Some(Operation::DeleteMachine(daemon::DeleteMachine {
                    name: "dev".into(),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Request(daemon::DaemonRequest {
                why: String::new(),
                id: 30,
                caption: String::new(),
                operation: Some(Operation::WatchActivity(daemon::WatchActivity {})),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 12,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "act s3e12 press".into(),
                    phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                        why: String::new(),
                        mark: None,
                        gesture: daemon::Gesture::Act.into(),
                        target: Some(daemon::call_started::Target::Element(
                            daemon::ElementTarget {
                                frame: Some(common::Rect {
                                    origin: Some(common::Point { x: 300.0, y: 400.0 }),
                                    size: Some(common::Size {
                                        width: 40.0,
                                        height: 30.0,
                                    }),
                                }),
                                label: "Equals".into(),
                            },
                        )),
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 13,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "drag".into(),
                    phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                        why: String::new(),
                        mark: None,
                        gesture: daemon::Gesture::Drag.into(),
                        target: Some(daemon::call_started::Target::Drag(daemon::DragTarget {
                            from: Some(common::Point { x: 1.0, y: 2.0 }),
                            to: Some(common::Point { x: 3.0, y: 4.0 }),
                        })),
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 14,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "mark (5, 6)".into(),
                    phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                        why: String::new(),
                        gesture: daemon::Gesture::Mark.into(),
                        target: None,
                        mark: Some(daemon::ScreenMark {
                            shape: daemon::MarkShape::Box.into(),
                            at: Some(daemon::screen_mark::At::Area(daemon::ElementTarget {
                                frame: Some(common::Rect {
                                    origin: Some(common::Point { x: 5.0, y: 6.0 }),
                                    size: Some(common::Size {
                                        width: 70.0,
                                        height: 20.0,
                                    }),
                                }),
                                label: "Total".into(),
                            })),
                            from: None,
                            note: "the total".into(),
                            lasts: Some(daemon::screen_mark::Lasts::ForMs(5000)),
                        }),
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 12,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "act s3e12 press".into(),
                    phase: Some(daemon::activity::Phase::Ended(daemon::CallEnded {
                        outcome: Some(daemon::call_ended::Outcome::TimedOut("no frame".into())),
                        seen: None,
                        refs: None,
                        read: None,
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 9,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "click".into(),
                    phase: Some(daemon::activity::Phase::Recent(daemon::CallRecent {
                        started: Some(daemon::CallStarted {
                            why: String::new(),
                            mark: None,
                            gesture: daemon::Gesture::Click.into(),
                            target: Some(daemon::call_started::Target::Point(common::Point {
                                x: 640.0,
                                y: 400.0,
                            })),
                        }),
                        ended: Some(daemon::CallEnded {
                            outcome: Some(daemon::call_ended::Outcome::Done(daemon::Done {})),
                            seen: None,
                            refs: None,
                            read: None,
                        }),
                        started_ms_ago: 4_200,
                        ended_ms_ago: Some(4_150),
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 14,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: "snapshot".into(),
                    phase: Some(daemon::activity::Phase::Ended(daemon::CallEnded {
                        outcome: Some(daemon::call_ended::Outcome::Done(daemon::Done {})),
                        seen: Some(agent::AxSnapshot {
                            snapshot_id: 3,
                            app: Some(agent::RunningApp {
                                pid: 412,
                                bundle_id: "com.apple.calculator".into(),
                                name: "Calculator".into(),
                            }),
                            nodes: vec![
                                agent::AxNode {
                                    role: "AXWindow".into(),
                                    title: "Calculator".into(),
                                    frame: Some(common::Rect {
                                        origin: Some(common::Point { x: 100.0, y: 80.0 }),
                                        size: Some(common::Size {
                                            width: 232.0,
                                            height: 390.0,
                                        }),
                                    }),
                                    ..agent::AxNode::default()
                                },
                                agent::AxNode {
                                    parent: Some(0),
                                    role: "AXButton".into(),
                                    description: "Equals".into(),
                                    frame: Some(common::Rect {
                                        origin: Some(common::Point { x: 280.0, y: 420.0 }),
                                        size: Some(common::Size {
                                            width: 48.0,
                                            height: 48.0,
                                        }),
                                    }),
                                    flags: Some(agent::AxFlags {
                                        enabled: Some(true),
                                        ..agent::AxFlags::default()
                                    }),
                                    actions: vec!["AXPress".into()],
                                    ..agent::AxNode::default()
                                },
                            ],
                            truncation: agent::Truncation::Complete.into(),
                        }),
                        refs: None,
                        read: None,
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Event(daemon::DaemonEvent {
                request: 30,
                kind: Some(daemon::daemon_event::Kind::Activity(daemon::Activity {
                    session: 4,
                    request: 15,
                    machine: "dev".into(),
                    client: "doppel mcp".into(),
                    caption: String::new(),
                    phase: Some(daemon::activity::Phase::Named(daemon::ElementRefs {
                        snapshot: 3,
                        generation: 3,
                        nodes: vec![1],
                    })),
                })),
            })),
        },
        daemon::DaemonFrame {
            kind: Some(Kind::Response(daemon::DaemonResponse {
                id: 31,
                handed_back: None,
                outcome: Some(Outcome::Recording(daemon::Recording {
                    id: "2026-11-14T09-01-02Z-s2".into(),
                    videos: vec![daemon::RecordingVideo {
                        file: "live-001.mp4".into(),
                        machine: "live".into(),
                        start_ms: Some(40),
                        display_points: Some(common::Size {
                            width: 1280.0,
                            height: 800.0,
                        }),
                    }],
                    trace: vec![daemon::TraceLine {
                        line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                            start_ms: 50,
                            end_ms: 80,
                            machine: Some("live".into()),
                            call: "click".into(),
                            outcome: daemon::TraceOutcome::Done as i32,
                            error: None,
                            waited_ms: None,
                            by: daemon::TraceActor::Session as i32,
                            queued_ms: None,
                            seen: true,
                            refs: Some(daemon::ElementRefs {
                                snapshot: 3,
                                generation: 3,
                                nodes: vec![1],
                            }),
                            read: Some(daemon::TextRead {
                                text: Some(vmm::RecognizedText {
                                    display_points: Some(common::Size {
                                        width: 1280.0,
                                        height: 800.0,
                                    }),
                                    lines: vec![vmm::TextLine {
                                        text: "Save as".into(),
                                        frame: Some(common::Rect {
                                            origin: Some(common::Point { x: 10.0, y: 10.0 }),
                                            size: Some(common::Size {
                                                width: 60.0,
                                                height: 18.0,
                                            }),
                                        }),
                                        confidence: 0.9,
                                        words: Vec::new(),
                                    }],
                                    accurate_failed: String::new(),
                                }),
                                query: Some("save".into()),
                                found: vec![common::Rect {
                                    origin: Some(common::Point { x: 10.0, y: 10.0 }),
                                    size: Some(common::Size {
                                        width: 30.0,
                                        height: 18.0,
                                    }),
                                }],
                            }),
                            acted: Some(daemon::CallStarted {
                                why: String::new(),
                                mark: None,
                                gesture: daemon::Gesture::Click.into(),
                                target: Some(daemon::call_started::Target::Point(common::Point {
                                    x: 640.0,
                                    y: 400.0,
                                })),
                            }),
                        })),
                    }],
                })),
            })),
        },
    ]
}

fn check<M: Envelope + PartialEq + std::fmt::Debug>(name: &str, frames: &[M]) {
    let encoder = FrameEncoder::new(MaxFrameLen::DEFAULT);
    let encoded: Vec<u8> = frames
        .iter()
        .flat_map(|f| encoder.encode(f).unwrap().to_vec())
        .collect();
    let path = fixture(name);

    #[expect(
        clippy::disallowed_methods,
        reason = "test-only switch to regenerate fixtures"
    )]
    let update = std::env::var("DOPPEL_UPDATE_GOLDEN").is_ok();
    if update {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &encoded).unwrap();
    }
    let committed = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; regenerate with DOPPEL_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        committed, encoded,
        "{name}: encoding changed; if intended, regenerate and update the Swift tests"
    );

    let mut decoder = FrameDecoder::<M>::new(MaxFrameLen::DEFAULT);
    decoder.push(&committed);
    let mut decoded = Vec::new();
    while let Some(frame) = decoder.next_frame().unwrap() {
        match frame {
            Received::Recognized { message, .. } => decoded.push(message),
            other @ (Received::Unrecognized { .. } | Received::Malformed { .. }) => {
                panic!("{other:?}")
            }
        }
    }
    assert_eq!(decoded, frames);
}

#[test]
fn agent_stream_matches_fixture() {
    check("agent_stream.v1.bin", &agent_stream());
}

#[test]
fn vmm_stream_matches_fixture() {
    check("vmm_stream.v1.bin", &vmm_stream());
}

#[test]
fn daemon_stream_matches_fixture() {
    check("daemon_stream.v1.bin", &daemon_stream());
}

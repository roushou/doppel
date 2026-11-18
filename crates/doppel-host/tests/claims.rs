//! Ownership and crash safety through the real shell: `Machines` on an APFS
//! temp directory, with a filesystem that fails at a chosen operation. After
//! each failure a fresh claim must settle the machine to the state before
//! the change or the state after it. The pure plans are crashed at every
//! step in `doppel-machine`; this checks the shell runs them that way.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::cell::Cell;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use doppel_host::capabilities::SystemClock;
use doppel_host::files::{FileOpError, Filesystem, LocalFilesystem};
use doppel_host::launcher::VmmBinary;
use doppel_host::machines::{ImportSource, MachineError, Machines};
use doppel_host::store::{Store, StoreError};
use doppel_machine::bundle::{DataRoot, FileOp, RuntimeDir};
use doppel_machine::lineage::{Lineage, Origin, RecordedLineage, Role, TemplateAgent};
use doppel_machine::machine::{
    CpuCount, DiskSize, DisplaySpec, Hardware, MachineName, MemorySize, NetworkSpec,
};
use doppel_machine::outcome::Unconfirmed;
use doppel_protocol::frame::{FrameDecoder, FrameEncoder, MaxFrameLen, Received};
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::vmm::v1 as vmm;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        // Short: the runtime directory holds unix socket paths.
        let dir = std::env::temp_dir().join(format!("dc-{tag}-{}", std::process::id()));
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

    fn machines<F: Filesystem>(&self, files: F) -> Machines<SystemClock, F> {
        Machines::new(
            self.store(),
            VmmBinary::new(self.0.join("no-vmm")),
            SystemClock,
            files,
        )
    }

    fn write(&self, path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The local filesystem, failing the `fail_at`th operation (from 0) as if
/// the process had died there: nothing after it runs.
struct FailAt {
    fail_at: usize,
    performed: Cell<usize>,
}

impl FailAt {
    const fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            performed: Cell::new(0),
        }
    }
}

impl Filesystem for FailAt {
    fn apply(&self, op: &FileOp) -> Result<(), FileOpError> {
        let n = self.performed.get();
        self.performed.set(n + 1);
        if n >= self.fail_at {
            return Err(FileOpError {
                op: "injected",
                path: PathBuf::new(),
                source: io::Error::other("injected failure"),
            });
        }
        LocalFilesystem.apply(op)
    }

    fn exists(&self, path: &Path) -> Result<bool, FileOpError> {
        LocalFilesystem.exists(path)
    }
}

fn name(s: &str) -> MachineName {
    s.parse().unwrap()
}

/// Every file under `dir`, relative, with its content; hidden staging
/// directories included, so leftovers show.
fn tree(dir: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, fs::read_to_string(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn a_machine_has_one_owner_at_a_time() {
    let scratch = Scratch::new("own");
    let machines = scratch.machines(LocalFilesystem);
    let dev = name("dev");
    let first = machines.claim(&dev, machines.deadline()).unwrap();
    assert!(matches!(
        machines.claim(&dev, machines.deadline()),
        Err(MachineError::Store(StoreError::Busy(n))) if n == dev
    ));
    // Another name is free, existing or not.
    let other = machines.claim(&name("other"), machines.deadline()).unwrap();
    drop(first);
    let _again = machines.claim(&dev, machines.deadline()).unwrap();
    drop(other);
}

#[test]
fn a_failed_restore_settles_to_before_or_after_never_a_mix() {
    let mut fail_at = 0;
    loop {
        let scratch = Scratch::new(&format!("rs{fail_at}"));
        let store = scratch.store();
        let bundle = store.bundle(&name("work"));
        let image = bundle.image();
        for (path, content) in [
            (&image.spec, "old-spec"),
            (&image.disk, "old-disk"),
            (&image.aux, "old-aux"),
            (&bundle.suspended_state(), "old-memory"),
            (&bundle.resume_attempt().kept().to_owned(), "old-kept"),
        ] {
            scratch.write(path, content);
        }
        let cp = bundle.checkpoint(&"clean".parse().unwrap());
        for (path, content) in [
            (&cp.image.spec, "cp-spec"),
            (&cp.image.disk, "cp-disk"),
            (&cp.image.aux, "cp-aux"),
            (&cp.memory, "cp-memory"),
        ] {
            scratch.write(path, content);
        }
        let image_now = || {
            [
                &image.spec,
                &image.disk,
                &image.aux,
                &bundle.suspended_state(),
                &bundle.resume_attempt().kept().to_owned(),
            ]
            .map(|p| fs::read_to_string(p).ok())
        };
        let before = image_now();
        let after = ["cp-spec", "cp-disk", "cp-aux", "cp-memory"]
            .map(|s| Some(s.to_owned()))
            .into_iter()
            .chain([None])
            .collect::<Vec<_>>();

        let failing = scratch.machines(FailAt::new(fail_at));
        let result = failing
            .claim(&name("work"), failing.deadline())
            .unwrap()
            .restore(&"clean".parse().unwrap(), failing.deadline());
        drop(failing);
        // The next owner settles whatever was left.
        let machines = scratch.machines(LocalFilesystem);
        drop(machines.claim(&name("work"), machines.deadline()).unwrap());
        let now = image_now();
        // A definite failure settles back; an unknown one either way, never
        // a mix (a marker that failed to appear leaves the old image).
        let settled_after = now.to_vec() == after;
        match &result {
            Ok(()) => assert!(settled_after, "failing op {fail_at}"),
            Err(Unconfirmed::Failed(_)) => assert_eq!(now, before, "failing op {fail_at}"),
            Err(Unconfirmed::Unknown(_)) => {
                assert!(
                    now == before || settled_after,
                    "failing op {fail_at}: {now:?}"
                );
            }
        }
        // Past the marker, the restore is decided: the next owner finishes it.
        if fail_at > 4 {
            assert!(settled_after, "failing op {fail_at}");
        }
        let leftovers: Vec<_> = tree(bundle.dir())
            .into_iter()
            .filter(|(p, _)| p.contains(".restoring") || p.contains("restore.commit"))
            .collect();
        assert!(leftovers.is_empty(), "failing op {fail_at}: {leftovers:?}");
        if result.is_ok() {
            assert_eq!(now.to_vec(), after);
            break;
        }
        fail_at += 1;
    }
    assert!(fail_at > 5, "every step was interrupted once ({fail_at})");
}

#[test]
fn a_failed_fork_leaves_no_machine_and_the_source_untouched() {
    let mut fail_at = 0;
    loop {
        let scratch = Scratch::new(&format!("fk{fail_at}"));
        let store = scratch.store();
        let source = store.bundle(&name("base"));
        let image = source.image();
        scratch.write(&image.spec, "base-spec");
        scratch.write(&image.disk, "base-disk");
        scratch.write(&image.aux, "base-aux");
        scratch.write(&source.login_password(), "secret");
        let cp = source.checkpoint(&"clean".parse().unwrap());
        scratch.write(&cp.image.spec, "cp-spec");
        scratch.write(&cp.image.disk, "cp-disk");
        scratch.write(&cp.image.aux, "cp-aux");
        scratch.write(&cp.memory, "cp-memory");
        let data = scratch.0.join("data");
        let source_before = tree(source.dir());

        let failing = scratch.machines(FailAt::new(fail_at));
        let result = failing
            .claim(&name("base"), failing.deadline())
            .unwrap()
            .fork(
                &"clean".parse().unwrap(),
                &failing.claim(&name("fork"), failing.deadline()).unwrap(),
            );
        drop(failing);
        let machines = scratch.machines(LocalFilesystem);
        drop(machines.claim(&name("fork"), machines.deadline()).unwrap());

        assert_eq!(tree(source.dir()), source_before);
        let fork = store.bundle(&name("fork"));
        let staging = data.join("machines/.fork.staging");
        assert!(!staging.exists(), "failing op {fail_at}");
        if result.is_ok() {
            let files: Vec<_> = tree(fork.dir())
                .into_iter()
                .filter(|(path, _)| path != "lineage.pb")
                .collect();
            assert_eq!(
                files,
                [
                    ("aux.bin", "cp-aux"),
                    ("disk.img", "cp-disk"),
                    ("login-password", "secret"),
                    ("spec.pb", "cp-spec"),
                    ("suspended.vzsave", "cp-memory"),
                ]
                .map(|(p, c)| (p.to_owned(), c.to_owned()))
            );
            assert_eq!(
                store.read_lineage(&fork),
                RecordedLineage::Known(Lineage::forked(name("base"), "clean".parse().unwrap()))
            );
            break;
        }
        assert!(!fork.dir().exists(), "failing op {fail_at}");
        fail_at += 1;
    }
    assert!(fail_at > 5, "every step was interrupted once ({fail_at})");
}

#[test]
fn a_fresh_fork_gets_its_own_mac_and_no_memory_or_nothing_at_all() {
    let spec =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundle/spec.v1.bin"))
            .unwrap();
    let mac = "02:00:00:00:00:2a".parse().unwrap();
    let mut fail_at = 0;
    loop {
        let scratch = Scratch::new(&format!("ff{fail_at}"));
        let store = scratch.store();
        let source = store.bundle(&name("base"));
        let image = source.image();
        fs::create_dir_all(source.dir()).unwrap();
        fs::write(&image.spec, &spec).unwrap();
        scratch.write(&image.disk, "base-disk");
        scratch.write(&image.aux, "base-aux");
        scratch.write(&source.login_password(), "secret");
        let cp = source.checkpoint(&"clean".parse().unwrap());
        fs::create_dir_all(cp.image.spec.parent().unwrap()).unwrap();
        fs::write(&cp.image.spec, &spec).unwrap();
        scratch.write(&cp.image.disk, "cp-disk");
        scratch.write(&cp.image.aux, "cp-aux");
        scratch.write(&cp.memory, "cp-memory");
        let source_before = tree(source.dir());
        let vmm = scratch.0.join("no-vmm");

        let failing = Machines::new(
            store.clone(),
            VmmBinary::new(vmm.clone()),
            SystemClock,
            FailAt::new(fail_at),
        );
        let result = failing
            .claim(&name("base"), failing.deadline())
            .unwrap()
            .fork_fresh(
                &"clean".parse().unwrap(),
                &failing
                    .claim(&name("base-clean-1"), failing.deadline())
                    .unwrap(),
                mac,
                Role::Temporary,
            );
        drop(failing);
        let machines = Machines::new(
            store.clone(),
            VmmBinary::new(vmm),
            SystemClock,
            LocalFilesystem,
        );
        drop(
            machines
                .claim(&name("base-clean-1"), machines.deadline())
                .unwrap(),
        );

        assert_eq!(tree(source.dir()), source_before);
        let fork = store.bundle(&name("base-clean-1"));
        assert!(
            !scratch
                .0
                .join("data/machines/.base-clean-1.staging")
                .exists(),
            "failing op {fail_at}"
        );
        if result.is_ok() {
            let files: Vec<_> = tree(fork.dir()).into_iter().map(|(path, _)| path).collect();
            assert_eq!(
                files,
                [
                    "aux.bin",
                    "disk.img",
                    "lineage.pb",
                    "login-password",
                    "spec.pb"
                ]
            );
            // Its role is in the lineage it appeared with.
            assert_eq!(
                store.read_lineage(&fork),
                RecordedLineage::Known(
                    Lineage::forked(name("base"), "clean".parse().unwrap())
                        .with_role(Role::Temporary)
                )
            );
            assert_eq!(fs::read_to_string(fork.image().disk).unwrap(), "cp-disk");
            let forked = store.read_spec(&fork).unwrap();
            let original = store.read_spec(&source).unwrap();
            assert_eq!(forked.network(), NetworkSpec::Nat { mac });
            assert_ne!(forked.network(), original.network());
            let doppel_machine::machine::Platform::Installed { identifier, .. } = forked.platform()
            else {
                panic!("not installed");
            };
            let doppel_machine::machine::Platform::Installed {
                identifier: source_identifier,
                ..
            } = original.platform()
            else {
                panic!("not installed");
            };
            assert_eq!(identifier, source_identifier);
            assert_eq!(
                (forked.cpus(), forked.disk()),
                (original.cpus(), original.disk())
            );
            break;
        }
        assert!(!fork.dir().exists(), "failing op {fail_at}");
        fail_at += 1;
    }
    assert!(fail_at > 3, "every step was interrupted once ({fail_at})");
}

#[test]
fn a_failed_import_leaves_no_machine_and_the_name_free() {
    let hardware = || Hardware {
        cpus: CpuCount::new(2).unwrap(),
        memory: MemorySize::from_gib(4).unwrap(),
        disk: DiskSize::from_gib(64).unwrap(),
        display: DisplaySpec::retina(1280, 800).unwrap(),
        network: NetworkSpec::None,
        shares: vec![],
    };
    let mut fail_at = 0;
    loop {
        let scratch = Scratch::new(&format!("im{fail_at}"));
        let src = scratch.0.join("src");
        let source = ImportSource {
            hardware_model: src.join("hw"),
            machine_identifier: src.join("id"),
            aux: src.join("aux"),
            disk: src.join("disk"),
        };
        for path in [
            &source.hardware_model,
            &source.machine_identifier,
            &source.aux,
            &source.disk,
        ] {
            scratch.write(path, "blob");
        }

        let failing = scratch.machines(FailAt::new(fail_at));
        let result = failing
            .claim(&name("adopted"), failing.deadline())
            .unwrap()
            .import(hardware(), &source);
        drop(failing);
        let machines = scratch.machines(LocalFilesystem);
        let claim = machines
            .claim(&name("adopted"), machines.deadline())
            .unwrap();
        let bundle = scratch.store().bundle(&name("adopted"));
        assert!(
            !scratch.0.join("data/machines/.adopted.staging").exists(),
            "failing op {fail_at}"
        );
        if result.is_ok() {
            assert_eq!(
                tree(bundle.dir())
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect::<Vec<_>>(),
                ["aux.bin", "disk.img", "lineage.pb", "spec.pb"]
            );
            assert_eq!(
                scratch.store().read_lineage(&bundle),
                RecordedLineage::Known(Lineage::made(Origin::Imported))
            );
            break;
        }
        assert!(!bundle.dir().exists(), "failing op {fail_at}");
        // The name is free again: a retry succeeds.
        claim.import(hardware(), &source).unwrap();
        fail_at += 1;
    }
    assert!(fail_at >= 3, "every step was interrupted once ({fail_at})");
}

/// One side of a VMM conversation: framed `VmmFrame`s on a unix socket.
struct FakeVmm {
    stream: std::os::unix::net::UnixStream,
    decoder: FrameDecoder<vmm::VmmFrame>,
}

impl FakeVmm {
    fn send(&mut self, kind: vmm::vmm_frame::Kind) {
        let frame = FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&vmm::VmmFrame { kind: Some(kind) })
            .unwrap();
        self.stream.write_all(&frame).unwrap();
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

    /// Accepts one client and greets it.
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

    /// Reads the next request, answering it with `outcome`; returns it.
    fn answer(&mut self, outcome: vmm::vmm_response::Outcome) -> vmm::vmm_request::Operation {
        let vmm::vmm_frame::Kind::Request(request) = self.receive() else {
            panic!("expected a request");
        };
        self.send(vmm::vmm_frame::Kind::Response(vmm::VmmResponse {
            id: request.id,
            outcome: Some(outcome),
        }));
        request.operation.unwrap()
    }
}

fn run_state(state: vmm::VmRunState) -> vmm::VmState {
    vmm::VmState {
        run_state: state as i32,
        window: vmm::WindowMode::Unspecified as i32,
    }
}

/// A doppel killed mid-checkpoint leaves its VMM saving into the staging
/// directory, with the guest paused. The next claim waits out the save
/// before removing the staging, then resumes the guest.
#[test]
fn a_claim_waits_out_an_interrupted_save_and_resumes_the_guest() {
    let scratch = Scratch::new("save");
    let store = scratch.store();
    let bundle = store.bundle(&name("work"));
    let staging = bundle.checkpoint_staging();
    scratch.write(&staging.memory, "half-saved");
    let listener = UnixListener::bind(store.socket(&name("work")).unwrap().path()).unwrap();

    let staging_dir = staging.dir.clone();
    let vmm_side = std::thread::spawn(move || {
        let mut vmm = FakeVmm::accept(&listener);
        let asked = vmm.answer(vmm::vmm_response::Outcome::State(run_state(
            vmm::VmRunState::Saving,
        )));
        assert!(matches!(asked, vmm::vmm_request::Operation::GetState(_)));
        std::thread::sleep(Duration::from_millis(200));
        assert!(staging_dir.exists(), "staging removed during the save");
        vmm.send(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
            kind: Some(vmm::vmm_event::Kind::StateChanged(run_state(
                vmm::VmRunState::Paused,
            ))),
        }));
        // The repair connects afresh.
        let mut repair = FakeVmm::accept(&listener);
        let asked = repair.answer(vmm::vmm_response::Outcome::Done(vmm::Done {}));
        assert!(matches!(asked, vmm::vmm_request::Operation::Resume(_)));
    });

    let machines = scratch.machines(LocalFilesystem);
    let _claim = machines.claim(&name("work"), machines.deadline()).unwrap();
    vmm_side.join().unwrap();
    assert!(!staging.dir.exists(), "the staging was settled");
}

/// A guest paused on purpose stays paused: there's no interrupted change.
#[test]
fn a_claim_leaves_a_guest_paused_on_purpose() {
    let scratch = Scratch::new("paused");
    let store = scratch.store();
    let listener = UnixListener::bind(store.socket(&name("work")).unwrap().path()).unwrap();
    let vmm_side = std::thread::spawn(move || {
        let mut vmm = FakeVmm::accept(&listener);
        vmm.answer(vmm::vmm_response::Outcome::State(run_state(
            vmm::VmRunState::Paused,
        )));
        // No further request: the claim closes the connection.
        let mut rest = Vec::new();
        vmm.stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
    });
    let machines = scratch.machines(LocalFilesystem);
    let _claim = machines.claim(&name("work"), machines.deadline()).unwrap();
    vmm_side.join().unwrap();
}

#[test]
fn a_role_is_recorded_beside_the_origin_and_an_unreadable_one_is_never_a_default() {
    let scratch = Scratch::new("role");
    let store = scratch.store();
    let machines = scratch.machines(LocalFilesystem);
    let dev = store.bundle(&name("dev"));
    let spec =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundle/spec.v1.bin"))
            .unwrap();
    fs::create_dir_all(dev.dir()).unwrap();
    fs::write(dev.image().spec, &spec).unwrap();
    let ready = dev.checkpoint(&"ready".parse().unwrap());
    scratch.write(&ready.image.disk, "cp-disk");
    scratch.write(&ready.memory, "cp-memory");

    // Made before lineage: a working machine of unknown origin.
    assert_eq!(
        store.read_lineage(&dev),
        RecordedLineage::Known(Lineage::default())
    );
    let claim = machines.claim(&name("dev"), machines.deadline()).unwrap();
    assert!(matches!(
        claim.mark_golden(&"missing".parse().unwrap(), TemplateAgent::Unrecorded),
        Err(MachineError::NoSuchCheckpoint { .. })
    ));
    assert!(!dev.lineage().exists(), "a refused mark writes nothing");
    let build: doppel_machine::agent::AgentBuild = "20261118T164320Z-7da947b".parse().unwrap();
    let golden = claim
        .mark_golden(
            &"ready".parse().unwrap(),
            TemplateAgent::Recorded(build.clone()),
        )
        .unwrap();
    assert_eq!(
        golden.role,
        Role::Golden {
            template: "ready".parse().unwrap(),
            agent: TemplateAgent::Recorded(build),
        }
    );
    assert_eq!(golden.origin, Origin::Unrecorded);
    assert_eq!(store.read_lineage(&dev), RecordedLineage::Known(golden));
    assert!(!dev.lineage().with_extension("pb.new").exists());
    claim.mark_working().unwrap();
    assert_eq!(
        store.read_lineage(&dev),
        RecordedLineage::Known(Lineage::default())
    );

    // A file that doesn't decode hides nothing and isn't overwritten.
    fs::write(dev.lineage(), b"\xff\xff").unwrap();
    let RecordedLineage::Unreadable(why) = store.read_lineage(&dev) else {
        panic!("read as known");
    };
    assert!(why.contains("lineage.pb"), "{why}");
    assert!(matches!(
        claim.mark_working(),
        Err(MachineError::LineageUnreadable { .. })
    ));
    assert_eq!(fs::read(dev.lineage()).unwrap(), b"\xff\xff");
}

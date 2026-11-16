//! Crash-at-every-step tests: each plan is interrupted before each of its
//! operations, then settled, with the settling itself interrupted at every
//! point and run again. The machine must end in the state before or the
//! state after, never a mix, and with nothing left to settle.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::model::Disk;
use super::{DataRoot, FileOp, KeptAux, MachineBundle};
use crate::login::Credentials;

fn bundle(name: &str) -> MachineBundle {
    DataRoot::new("/data".into())
        .unwrap()
        .machine(&name.parse().unwrap())
}

/// One step of a change: a file operation, or a write the shell does itself.
#[derive(Debug, Clone)]
enum Step {
    Op(FileOp),
    Write(PathBuf, &'static str),
}

impl Step {
    fn ops(ops: Vec<FileOp>) -> Vec<Self> {
        ops.into_iter().map(Self::Op).collect()
    }

    fn perform(&self, disk: &mut Disk) {
        match self {
            Self::Op(op) => disk.apply(op).unwrap_or_else(|e| panic!("{op:?}: {e}")),
            Self::Write(path, content) => disk.write(path.clone(), content),
        }
    }
}

/// Settles `b`, as the shell does before changing it, interrupted after
/// `cut` operations; returns how many operations the full settle had.
fn settle(disk: &mut Disk, b: &MachineBundle, cut: usize) -> usize {
    let leftovers = b.leftovers();
    let ops = leftovers.settle(&disk.present(&leftovers.probes()));
    disk.run(&ops[..cut.min(ops.len())]).unwrap();
    ops.len()
}

/// Every interruption of `steps`, each settled with every interruption of
/// the settling. Calls `check(crash_point, disk)` on each settled result.
fn every_interruption(
    start: &Disk,
    steps: &[Step],
    settled: &[&MachineBundle],
    check: impl Fn(usize, &Disk),
) {
    for crash in 0..=steps.len() {
        let mut interrupted = start.clone();
        for step in &steps[..crash] {
            step.perform(&mut interrupted);
        }
        let mut cut = 0;
        loop {
            let mut disk = interrupted.clone();
            let mut full = 0;
            for b in settled {
                full = full.max(settle(&mut disk, b, cut));
            }
            for b in settled {
                settle(&mut disk, b, usize::MAX);
            }
            for b in settled {
                let leftovers = b.leftovers();
                let present = disk.present(&leftovers.probes());
                assert!(
                    leftovers.settle(&present).is_empty(),
                    "crash {crash}, cut {cut}: still unsettled: {present:?}"
                );
            }
            check(crash, &disk);
            if cut >= full {
                break;
            }
            cut += 1;
        }
    }
}

fn image(disk: &Disk, b: &MachineBundle) -> [Option<String>; 5] {
    let i = b.image();
    [
        i.spec,
        i.disk,
        i.aux,
        b.suspended_state(),
        b.resume_attempt().kept().to_owned(),
    ]
    .map(|p| disk.read(&p).map(str::to_owned))
}

fn with_checkpoint(disk: &mut Disk, b: &MachineBundle, label: &str) {
    let cp = b.checkpoint(&label.parse().unwrap());
    disk.write(cp.image.spec, "cp-spec");
    disk.write(cp.image.disk, "cp-disk");
    disk.write(cp.image.aux, "cp-aux");
    disk.write(cp.memory, "cp-memory");
}

#[test]
fn an_interrupted_restore_is_never_half_applied() {
    let b = bundle("work");
    for suspended in [false, true] {
        for kept in [KeptAux::Absent, KeptAux::Present] {
            let mut start = Disk::default();
            let i = b.image();
            start.write(i.spec, "old-spec");
            start.write(i.disk, "old-disk");
            start.write(i.aux, "old-aux");
            if suspended {
                start.write(b.suspended_state(), "old-memory");
            }
            if kept == KeptAux::Present {
                start.write(b.resume_attempt().kept().to_owned(), "old-kept");
            }
            with_checkpoint(&mut start, &b, "clean");
            let before = image(&start, &b);
            let after = ["cp-spec", "cp-disk", "cp-aux", "cp-memory"]
                .map(|s| Some(s.to_owned()))
                .into_iter()
                .chain([None])
                .collect::<Vec<_>>();
            let ops = b
                .restore_from(&b.checkpoint(&"clean".parse().unwrap()), kept)
                .into_ops();
            let steps = Step::ops(ops);
            let last = steps.len();
            every_interruption(&start, &steps, &[&b], |crash, disk| {
                let now = image(disk, &b);
                let label = format!("suspended {suspended}, kept {kept:?}, crash {crash}");
                if crash == 0 {
                    assert_eq!(now, before, "{label}");
                } else if crash == last {
                    assert_eq!(now.to_vec(), after, "{label}");
                } else {
                    assert!(now == before || now.to_vec() == after, "{label}: {now:?}");
                }
            });
        }
    }
}

#[test]
fn an_interrupted_fork_either_never_appears_or_is_complete() {
    let source = bundle("base");
    let into = bundle("fork");
    for login in [Credentials::None, Credentials::Stored] {
        let mut start = Disk::default();
        let i = source.image();
        start.write(i.spec, "base-spec");
        start.write(i.disk, "base-disk");
        start.write(i.aux, "base-aux");
        start.write(source.login_password(), "secret");
        with_checkpoint(&mut start, &source, "clean");
        let source_files = start.files_under(source.dir());

        let mut complete: BTreeMap<PathBuf, String> = [
            ("spec.pb", "cp-spec"),
            ("disk.img", "cp-disk"),
            ("aux.bin", "cp-aux"),
            ("suspended.vzsave", "cp-memory"),
            ("lineage.pb", "lineage"),
        ]
        .map(|(p, c)| (PathBuf::from(p), c.to_owned()))
        .into();
        if login == Credentials::Stored {
            complete.insert("login-password".into(), "secret".into());
        }

        let cp = source.checkpoint(&"clean".parse().unwrap());
        let fork = source.fork_from(&cp, &into, login);
        let mut steps = Step::ops(fork.build);
        steps.push(Step::Write(fork.lineage, "lineage"));
        steps.push(Step::Op(fork.commit));
        let last = steps.len();
        every_interruption(&start, &steps, &[&source, &into], |crash, disk| {
            assert_eq!(disk.files_under(source.dir()), source_files);
            if crash == last {
                assert_eq!(disk.files_under(into.dir()), complete, "crash {crash}");
            } else {
                assert!(!disk.any_under(into.dir()), "crash {crash}");
            }
        });
    }
}

#[test]
fn an_interrupted_import_either_never_appears_or_is_complete() {
    let into = bundle("adopted");
    let mut start = Disk::default();
    start.write("/elsewhere/aux", "their-aux");
    start.write("/elsewhere/disk", "their-disk");
    let assembly = into.import_from("/elsewhere/aux".as_ref(), "/elsewhere/disk".as_ref());
    let mut steps = Step::ops(assembly.build);
    steps.push(Step::Write(assembly.spec, "spec"));
    steps.push(Step::Write(assembly.lineage, "lineage"));
    steps.push(Step::Op(assembly.commit));
    let last = steps.len();
    every_interruption(&start, &steps, &[&into], |crash, disk| {
        assert_eq!(disk.read("/elsewhere/disk".as_ref()), Some("their-disk"));
        if crash == last {
            assert_eq!(
                disk.files_under(into.dir()).into_keys().collect::<Vec<_>>(),
                ["aux.bin", "disk.img", "lineage.pb", "spec.pb"].map(PathBuf::from)
            );
        } else {
            assert!(!disk.any_under(into.dir()), "crash {crash}");
        }
    });
}

#[test]
fn an_interrupted_checkpoint_delete_leaves_it_whole_or_gone() {
    let b = bundle("work");
    let mut start = Disk::default();
    let i = b.image();
    start.write(i.spec, "spec");
    start.write(i.disk, "disk");
    start.write(i.aux, "aux");
    with_checkpoint(&mut start, &b, "stale");
    with_checkpoint(&mut start, &b, "keep");
    let stale = b.checkpoint(&"stale".parse().unwrap()).dir;
    let whole = start.files_under(&stale);
    let keep = b.checkpoint(&"keep".parse().unwrap()).dir;
    let kept = start.files_under(&keep);
    let before = image(&start, &b);
    let steps = Step::ops(b.delete_checkpoint(&"stale".parse().unwrap()));
    every_interruption(&start, &steps, &[&b], |crash, disk| {
        assert_eq!(image(disk, &b), before, "crash {crash}");
        assert_eq!(disk.files_under(&keep), kept, "crash {crash}");
        let now = disk.files_under(&stale);
        if crash == 0 {
            assert_eq!(now, whole, "crash {crash}");
        } else {
            assert!(now.is_empty(), "crash {crash}: {now:?}");
        }
        assert!(
            !disk.any_under(&b.checkpoints_dir().join(".deleting")),
            "crash {crash}"
        );
    });
}

//! Taking a checkpoint of a running machine.
//!
//! Memory and disk must describe the same instant, or the restored guest sees
//! a disk that changed under it. The procedure:
//!
//! ```text
//! sync guest → pause → flush disk+aux → clone spec+disk+aux → save memory
//!   → commit → resume | stop
//! ```
//!
//! Files are written to the machine's checkpoint staging directory and the
//! commit renames it to the label, so a checkpoint under its label is always
//! complete. An interrupted one is left in staging, where
//! [`crate::bundle::Leftovers`] removes it.
//!
//! The guest's own sync comes first: writes it still holds in memory are on
//! its disk when the disk is cloned, so a cold boot from the checkpoint (or
//! from the source after a stop) sees them. Without it, settings written seconds
//! before (auto-login's, for one) can be missing from the clone. The sync
//! needs the guest agent; without one the checkpoint goes ahead and says so.
//!
//! Written as a pure state machine, `(Checkpoint, Outcome) -> (Checkpoint, Actions)`.
//! The shell performs each [`Action`] and feeds back its [`Outcome`]: the
//! [`Event`] it produced, or why it didn't. Definite failures resume the guest
//! rather than leave it paused; an unknown outcome (a lost VMM connection, a
//! timeout) ends in [`Checkpoint::Unknown`], never in success.
//!
//! `E` is the shell's error. The procedure never inspects it, only carries it
//! to the report.

mod result;

pub use result::{CheckpointResult, GuestAfter};

use crate::bundle::{CheckpointFiles, CheckpointLabel, FileOp, ImageFiles, MachineBundle};
use doppel_desktop::exec::Termination;
use doppel_desktop::outcome::{Outcome, Unconfirmed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterCheckpoint {
    Resume,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPlan {
    source: ImageFiles,
    staging: CheckpointFiles,
    target: CheckpointFiles,
    then: AfterCheckpoint,
}

impl CheckpointPlan {
    #[must_use]
    pub fn new(machine: &MachineBundle, label: &CheckpointLabel, then: AfterCheckpoint) -> Self {
        Self {
            source: machine.image(),
            staging: machine.checkpoint_staging(),
            target: machine.checkpoint(label),
            then,
        }
    }
    #[must_use]
    pub const fn target(&self) -> &CheckpointFiles {
        &self.target
    }
}

/// Whether the guest wrote its cached data to its disk before the pause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestSync<E> {
    Synced,
    /// The checkpoint is valid for resuming; a cold boot from its disk may
    /// lack the guest's latest writes.
    Skipped(SyncSkipped<E>),
}

/// Why the guest wasn't synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSkipped<E> {
    /// `sync` ran in the guest but didn't exit cleanly; its stderr, trimmed.
    Ended {
        termination: Termination,
        stderr: String,
    },
    /// It couldn't be run: no agent (the login window), or the request failed.
    NotRun(E),
}

impl<E> GuestSync<E> {
    /// The same result with each error converted, such as for a report.
    pub fn map_error<F>(self, f: impl Fn(E) -> F) -> GuestSync<F> {
        match self {
            Self::Synced => GuestSync::Synced,
            Self::Skipped(SyncSkipped::Ended {
                termination,
                stderr,
            }) => GuestSync::Skipped(SyncSkipped::Ended {
                termination,
                stderr,
            }),
            Self::Skipped(SyncSkipped::NotRun(error)) => {
                GuestSync::Skipped(SyncSkipped::NotRun(f(error)))
            }
        }
    }
}

impl<E: std::fmt::Display> GuestSync<E> {
    /// What a report should add: nothing when synced.
    #[must_use]
    pub fn caveat(&self) -> String {
        match self {
            Self::Synced => String::new(),
            Self::Skipped(why) => Self::skipped_caveat(why),
        }
    }

    /// The caveat for a sync skipped because of `why`: for a report that
    /// carries only the reason, such as one received from the daemon.
    #[must_use]
    pub fn skipped_caveat(why: &dyn std::fmt::Display) -> String {
        format!(
            "; the guest wasn't synced ({why}), so a cold boot from this disk may lack its latest writes"
        )
    }
}

impl<E: std::fmt::Display> std::fmt::Display for SyncSkipped<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ended {
                termination,
                stderr,
            } => {
                write!(f, "sync {termination}")?;
                if stderr.is_empty() {
                    Ok(())
                } else {
                    write!(f, ": {stderr}")
                }
            }
            Self::NotRun(error) => error.fmt(f),
        }
    }
}

/// What the procedure is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    SyncingGuest,
    Pausing,
    Flushing,
    Cloning,
    SavingMemory,
    Committing,
    Finishing,
}

/// Requests for the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Ask the guest to write cached data to its disk. Never fails the
    /// checkpoint: the shell reports a skip instead.
    SyncGuest,
    PauseVm,
    Files(Vec<FileOp>),
    SaveMemory {
        path: std::path::PathBuf,
    },
    ResumeVm,
    StopVm,
    /// Remove the uncommitted staging directory.
    Discard {
        dir: std::path::PathBuf,
    },
}

/// What a performed [`Action`] produced. Its failures are the
/// [`Unconfirmed`] side of the [`Outcome`] the shell reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<E> {
    GuestSynced(GuestSync<E>),
    Paused,
    FilesDone,
    MemorySaved { bytes: u64 },
    Resumed,
    Stopped,
}

/// How the guest was left after a completed checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aftermath<E> {
    AsRequested(AfterCheckpoint),
    /// The checkpoint is complete and valid, but the guest may still be
    /// paused: the resume or stop failed, or its outcome is unknown.
    LeftPaused(Unconfirmed<E>),
}

/// What happened to the guest after a failed checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestAfterFailure {
    /// Failed before pausing: the guest never stopped running.
    Untouched,
    /// A resume was requested. Its result arrives as a later event.
    ResumeRequested,
}

/// What a checkpoint has saved once its memory is: everything a completed
/// one reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved<E> {
    pub guest_sync: GuestSync<E>,
    pub memory_bytes: u64,
}

/// The step an in-progress checkpoint awaits, with what the steps before it
/// learned: the guest sync once it has answered, the memory once saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Awaiting<E> {
    SyncingGuest,
    Pausing(GuestSync<E>),
    Flushing(GuestSync<E>),
    Cloning(GuestSync<E>),
    SavingMemory(GuestSync<E>),
    Committing(Saved<E>),
    Finishing(Saved<E>),
}

impl<E> Awaiting<E> {
    #[must_use]
    pub const fn step(&self) -> Step {
        match self {
            Self::SyncingGuest => Step::SyncingGuest,
            Self::Pausing(_) => Step::Pausing,
            Self::Flushing(_) => Step::Flushing,
            Self::Cloning(_) => Step::Cloning,
            Self::SavingMemory(_) => Step::SavingMemory,
            Self::Committing(_) => Step::Committing,
            Self::Finishing(_) => Step::Finishing,
        }
    }

    fn map_error<F>(self, f: impl Fn(E) -> F) -> Awaiting<F> {
        let saved = |saved: Saved<E>| Saved {
            guest_sync: saved.guest_sync.map_error(&f),
            memory_bytes: saved.memory_bytes,
        };
        match self {
            Self::SyncingGuest => Awaiting::SyncingGuest,
            Self::Pausing(sync) => Awaiting::Pausing(sync.map_error(&f)),
            Self::Flushing(sync) => Awaiting::Flushing(sync.map_error(&f)),
            Self::Cloning(sync) => Awaiting::Cloning(sync.map_error(&f)),
            Self::SavingMemory(sync) => Awaiting::SavingMemory(sync.map_error(&f)),
            Self::Committing(done) => Awaiting::Committing(saved(done)),
            Self::Finishing(done) => Awaiting::Finishing(saved(done)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkpoint<E> {
    InProgress {
        plan: CheckpointPlan,
        awaiting: Awaiting<E>,
    },
    Completed {
        plan: CheckpointPlan,
        guest_sync: GuestSync<E>,
        memory_bytes: u64,
        aftermath: Aftermath<E>,
    },
    Failed {
        plan: CheckpointPlan,
        at: Step,
        error: E,
        guest: GuestAfterFailure,
    },
    /// Outcome unknown: neither success nor failure can be claimed.
    Unknown {
        plan: CheckpointPlan,
        at: Step,
        error: E,
    },
}

impl<E> Checkpoint<E> {
    /// The same state with each error it carries converted, such as for a
    /// report in another error type.
    pub fn map_error<F>(self, f: impl Fn(E) -> F) -> Checkpoint<F> {
        match self {
            Self::InProgress { plan, awaiting } => Checkpoint::InProgress {
                plan,
                awaiting: awaiting.map_error(&f),
            },
            Self::Completed {
                plan,
                guest_sync,
                memory_bytes,
                aftermath,
            } => Checkpoint::Completed {
                plan,
                guest_sync: guest_sync.map_error(&f),
                memory_bytes,
                aftermath: match aftermath {
                    Aftermath::AsRequested(then) => Aftermath::AsRequested(then),
                    Aftermath::LeftPaused(why) => Aftermath::LeftPaused(why.map(&f)),
                },
            },
            Self::Failed {
                plan,
                at,
                error,
                guest,
            } => Checkpoint::Failed {
                plan,
                at,
                error: f(error),
                guest,
            },
            Self::Unknown { plan, at, error } => Checkpoint::Unknown {
                plan,
                at,
                error: f(error),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{answer:?} is not valid in checkpoint state {state}")]
pub struct IllegalTransition<E: std::fmt::Debug> {
    pub state: &'static str,
    pub answer: Outcome<Event<E>, E>,
}

impl<E: std::fmt::Debug> Checkpoint<E> {
    #[must_use]
    pub fn begin(plan: CheckpointPlan) -> (Self, Vec<Action>) {
        (
            Self::InProgress {
                plan,
                awaiting: Awaiting::SyncingGuest,
            },
            vec![Action::SyncGuest],
        )
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::InProgress { .. } => false,
            Self::Completed { .. } | Self::Failed { .. } | Self::Unknown { .. } => true,
        }
    }

    const fn state_name(&self) -> &'static str {
        match self {
            Self::InProgress { .. } => "in progress",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Unknown { .. } => "unknown",
        }
    }

    /// # Errors
    /// [`IllegalTransition`] for an answer the current state can't receive,
    /// including any answer after a terminal state.
    pub fn on(
        self,
        answer: Outcome<Event<E>, E>,
    ) -> Result<(Self, Vec<Action>), IllegalTransition<E>> {
        let illegal = |state: &Self, answer: Outcome<Event<E>, E>| IllegalTransition {
            state: state.state_name(),
            answer,
        };
        let (plan, awaiting) = match self {
            Self::InProgress { plan, awaiting } => (plan, awaiting),
            terminal @ (Self::Completed { .. } | Self::Failed { .. } | Self::Unknown { .. }) => {
                return Err(illegal(&terminal, answer));
            }
        };
        let progress = |plan, awaiting| Self::InProgress { plan, awaiting };

        Ok(match (awaiting, answer) {
            (Awaiting::SyncingGuest, Ok(Event::GuestSynced(sync))) => (
                progress(plan, Awaiting::Pausing(sync)),
                vec![Action::PauseVm],
            ),
            (Awaiting::Pausing(sync), Ok(Event::Paused)) => {
                let flush = [&plan.source.disk, &plan.source.aux]
                    .map(|p| FileOp::Flush { path: p.clone() });
                (
                    progress(plan, Awaiting::Flushing(sync)),
                    vec![Action::Files(flush.into())],
                )
            }
            (Awaiting::Flushing(sync), Ok(Event::FilesDone)) => {
                let t = &plan.staging;
                let mut ops = vec![FileOp::CreateDir {
                    path: t.dir.clone(),
                }];
                for (from, to) in [
                    (&plan.source.spec, &t.image.spec),
                    (&plan.source.disk, &t.image.disk),
                    (&plan.source.aux, &t.image.aux),
                ] {
                    ops.push(FileOp::Clone {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
                (
                    progress(plan, Awaiting::Cloning(sync)),
                    vec![Action::Files(ops)],
                )
            }
            (Awaiting::Cloning(sync), Ok(Event::FilesDone)) => {
                let path = plan.staging.memory.clone();
                (
                    progress(plan, Awaiting::SavingMemory(sync)),
                    vec![Action::SaveMemory { path }],
                )
            }
            (Awaiting::SavingMemory(guest_sync), Ok(Event::MemorySaved { bytes })) => {
                let commit = vec![
                    FileOp::Flush {
                        path: plan.staging.memory.clone(),
                    },
                    FileOp::Rename {
                        from: plan.staging.dir.clone(),
                        to: plan.target.dir.clone(),
                    },
                ];
                let saved = Saved {
                    guest_sync,
                    memory_bytes: bytes,
                };
                (
                    progress(plan, Awaiting::Committing(saved)),
                    vec![Action::Files(commit)],
                )
            }
            (Awaiting::Committing(saved), Ok(Event::FilesDone)) => {
                let next = match plan.then {
                    AfterCheckpoint::Resume => Action::ResumeVm,
                    AfterCheckpoint::Stop => Action::StopVm,
                };
                (progress(plan, Awaiting::Finishing(saved)), vec![next])
            }
            (Awaiting::Finishing(saved), Ok(done @ (Event::Resumed | Event::Stopped))) => {
                let as_asked = matches!(
                    (plan.then, &done),
                    (AfterCheckpoint::Resume, Event::Resumed)
                        | (AfterCheckpoint::Stop, Event::Stopped)
                );
                if !as_asked {
                    return Err(illegal(
                        &progress(plan, Awaiting::Finishing(saved)),
                        Ok(done),
                    ));
                }
                let then = plan.then;
                (
                    Self::Completed {
                        plan,
                        guest_sync: saved.guest_sync,
                        memory_bytes: saved.memory_bytes,
                        aftermath: Aftermath::AsRequested(then),
                    },
                    vec![],
                )
            }

            // Files and memory are committed, so a resume or stop that failed,
            // or whose outcome is unknown, still leaves a valid checkpoint.
            (Awaiting::Finishing(saved), Err(unconfirmed)) => (
                Self::Completed {
                    plan,
                    guest_sync: saved.guest_sync,
                    memory_bytes: saved.memory_bytes,
                    aftermath: Aftermath::LeftPaused(unconfirmed),
                },
                vec![],
            ),
            (
                awaiting @ (Awaiting::SyncingGuest | Awaiting::Pausing(_)),
                Err(Unconfirmed::Failed(error)),
            ) => (
                Self::Failed {
                    plan,
                    at: awaiting.step(),
                    error,
                    guest: GuestAfterFailure::Untouched,
                },
                vec![],
            ),
            (
                awaiting @ (Awaiting::Flushing(_)
                | Awaiting::Cloning(_)
                | Awaiting::SavingMemory(_)
                | Awaiting::Committing(_)),
                Err(Unconfirmed::Failed(error)),
            ) => {
                let at = awaiting.step();
                let dir = plan.staging.dir.clone();
                let mut actions = vec![Action::ResumeVm];
                // Flushing created nothing yet; later steps may have.
                if at != Step::Flushing {
                    actions.insert(0, Action::Discard { dir });
                }
                (
                    Self::Failed {
                        plan,
                        at,
                        error,
                        guest: GuestAfterFailure::ResumeRequested,
                    },
                    actions,
                )
            }

            // The guest may be paused, the memory half saved: claim nothing,
            // and leave the staging for the next claim to settle.
            (
                awaiting @ (Awaiting::SyncingGuest
                | Awaiting::Pausing(_)
                | Awaiting::Flushing(_)
                | Awaiting::Cloning(_)
                | Awaiting::SavingMemory(_)
                | Awaiting::Committing(_)),
                Err(Unconfirmed::Unknown(error)),
            ) => (
                Self::Unknown {
                    plan,
                    at: awaiting.step(),
                    error,
                },
                vec![],
            ),

            (
                awaiting @ (Awaiting::SyncingGuest
                | Awaiting::Pausing(_)
                | Awaiting::Flushing(_)
                | Awaiting::Cloning(_)
                | Awaiting::SavingMemory(_)
                | Awaiting::Committing(_)
                | Awaiting::Finishing(_)),
                Ok(
                    event @ (Event::GuestSynced(_)
                    | Event::Paused
                    | Event::FilesDone
                    | Event::MemorySaved { .. }
                    | Event::Resumed
                    | Event::Stopped),
                ),
            ) => {
                return Err(illegal(&progress(plan, awaiting), Ok(event)));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::DataRoot;

    fn plan(then: AfterCheckpoint) -> CheckpointPlan {
        let bundle = DataRoot::new("/d".into())
            .unwrap()
            .machine(&"m".parse().unwrap());
        CheckpointPlan::new(&bundle, &"cp".parse().unwrap(), then)
    }

    /// Drives the machine and records every action.
    fn run(
        then: AfterCheckpoint,
        events: Vec<Outcome<Event<String>, String>>,
    ) -> (Checkpoint<String>, Vec<Action>) {
        let (mut state, mut actions) = Checkpoint::begin(plan(then));
        for event in events {
            let (next, more) = state.on(event).unwrap();
            state = next;
            actions.extend(more);
        }
        (state, actions)
    }

    #[test]
    fn happy_path_pauses_flushes_clones_saves_then_resumes() {
        let (state, actions) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 7 }),
                Ok(Event::FilesDone),
                Ok(Event::Resumed),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Completed {
                memory_bytes: 7,
                aftermath: Aftermath::AsRequested(AfterCheckpoint::Resume),
                ..
            }
        ));
        let kinds: Vec<_> = actions
            .iter()
            .map(|a| match a {
                Action::SyncGuest => "sync",
                Action::PauseVm => "pause",
                Action::Files(ops) if matches!(ops.last(), Some(FileOp::Rename { .. })) => "commit",
                Action::Files(ops) if matches!(ops[0], FileOp::Flush { .. }) => "flush",
                Action::Files(_) => "clone",
                Action::SaveMemory { .. } => "save",
                Action::ResumeVm => "resume",
                Action::StopVm => "stop",
                Action::Discard { .. } => "discard",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "sync", "pause", "flush", "clone", "save", "commit", "resume"
            ]
        );
    }

    #[test]
    fn disk_is_flushed_before_it_is_cloned_and_cloned_before_memory_is_saved() {
        let (_, actions) = run(
            AfterCheckpoint::Stop,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
            ],
        );
        let Action::Files(flush) = &actions[2] else {
            panic!()
        };
        assert!(flush.contains(&FileOp::Flush {
            path: "/d/machines/m/disk.img".into()
        }));
        let Action::Files(clone) = &actions[3] else {
            panic!()
        };
        assert!(clone.contains(&FileOp::Clone {
            from: "/d/machines/m/disk.img".into(),
            to: "/d/machines/m/checkpoints/.staging/disk.img".into()
        }));
        assert_eq!(
            actions[4],
            Action::SaveMemory {
                path: "/d/machines/m/checkpoints/.staging/memory.vzsave".into()
            }
        );
    }

    #[test]
    fn stop_mode_ends_with_stop() {
        let (state, actions) = run(
            AfterCheckpoint::Stop,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
                Ok(Event::FilesDone),
                Ok(Event::Stopped),
            ],
        );
        assert_eq!(actions.last(), Some(&Action::StopVm));
        assert!(matches!(
            state,
            Checkpoint::Completed {
                aftermath: Aftermath::AsRequested(AfterCheckpoint::Stop),
                ..
            }
        ));
    }

    #[test]
    fn failure_after_pausing_discards_partial_files_and_resumes_guest() {
        let (state, actions) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Err(Unconfirmed::Failed("ENOSPC".into())),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Failed {
                at: Step::Cloning,
                guest: GuestAfterFailure::ResumeRequested,
                ..
            }
        ));
        assert_eq!(
            &actions[actions.len() - 2..],
            [
                Action::Discard {
                    dir: "/d/machines/m/checkpoints/.staging".into()
                },
                Action::ResumeVm
            ]
        );
    }

    #[test]
    fn failure_to_pause_leaves_guest_untouched() {
        let (state, actions) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Err(Unconfirmed::Failed("not running".into())),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Failed {
                at: Step::Pausing,
                guest: GuestAfterFailure::Untouched,
                ..
            }
        ));
        assert_eq!(actions, [Action::SyncGuest, Action::PauseVm]);
    }

    #[test]
    fn failed_resume_still_yields_a_valid_checkpoint() {
        let (state, _) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 3 }),
                Ok(Event::FilesDone),
                Err(Unconfirmed::Failed("busy".into())),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Completed {
                aftermath: Aftermath::LeftPaused { .. },
                ..
            }
        ));
    }

    #[test]
    fn an_unknown_resume_completes_the_checkpoint_but_not_the_aftermath() {
        let (state, _) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 3 }),
                Ok(Event::FilesDone),
                Err(Unconfirmed::Unknown("timed out".into())),
            ],
        );
        let Checkpoint::Completed {
            aftermath: Aftermath::LeftPaused(why),
            ..
        } = state
        else {
            panic!("committed files make a checkpoint: {state:?}")
        };
        assert_eq!(why, Unconfirmed::Unknown("timed out".to_owned()));
    }

    #[test]
    fn an_unknown_pause_is_unknown_and_nothing_is_resumed_blindly() {
        let (state, actions) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Err(Unconfirmed::Unknown("connection lost".into())),
            ],
        );
        assert_eq!(
            state,
            Checkpoint::Unknown {
                plan: plan(AfterCheckpoint::Resume),
                at: Step::Pausing,
                error: "connection lost".to_owned(),
            }
        );
        assert_eq!(actions, [Action::SyncGuest, Action::PauseVm]);
    }

    #[test]
    fn lost_connection_is_unknown_never_success() {
        let (state, _) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Err(Unconfirmed::Unknown("connection lost".into())),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Unknown {
                at: Step::SavingMemory,
                ..
            }
        ));
    }

    #[test]
    fn out_of_order_and_post_terminal_events_are_rejected() {
        let (state, _) = Checkpoint::<String>::begin(plan(AfterCheckpoint::Resume));
        assert!(state.clone().on(Ok(Event::FilesDone)).is_err());
        assert!(
            state.clone().on(Ok(Event::Paused)).is_err(),
            "the guest syncs before the pause"
        );
        assert!(state.on(Ok(Event::MemorySaved { bytes: 1 })).is_err());

        let (done, _) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
                Ok(Event::FilesDone),
                Ok(Event::Resumed),
            ],
        );
        assert!(done.is_terminal());
        assert!(done.on(Ok(Event::Resumed)).is_err());

        // Stopped when a resume was asked for is not success.
        let (finishing, _) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
                Ok(Event::FilesDone),
            ],
        );
        assert!(finishing.on(Ok(Event::Stopped)).is_err());
    }

    #[test]
    fn the_guest_syncs_before_the_pause_and_a_skip_is_reported_not_hidden() {
        let skipped = GuestSync::Skipped(SyncSkipped::NotRun(
            "no agent at the login window".to_owned(),
        ));
        let (state, actions) = run(
            AfterCheckpoint::Stop,
            vec![
                Ok(Event::GuestSynced(skipped.clone())),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
                Ok(Event::FilesDone),
                Ok(Event::Stopped),
            ],
        );
        assert_eq!(actions[..2], [Action::SyncGuest, Action::PauseVm]);
        let Checkpoint::Completed { guest_sync, .. } = state else {
            panic!("expected a completed checkpoint, got {state:?}")
        };
        assert_eq!(
            guest_sync, skipped,
            "a skipped sync still completes, and says so"
        );
        assert!(guest_sync.caveat().contains("no agent at the login window"));
        assert_eq!(GuestSync::<String>::Synced.caveat(), "");
        let ended = |stderr: &str| {
            GuestSync::<String>::Skipped(SyncSkipped::Ended {
                termination: Termination::Exited(1),
                stderr: stderr.to_owned(),
            })
        };
        assert!(ended("").caveat().contains("(sync exited with status 1)"));
        assert!(
            ended("Input/output error")
                .caveat()
                .contains("(sync exited with status 1: Input/output error)")
        );
    }

    #[test]
    fn a_failure_while_syncing_leaves_the_guest_untouched() {
        let (state, actions) = run(
            AfterCheckpoint::Resume,
            vec![Err(Unconfirmed::Failed("x".into()))],
        );
        assert!(matches!(
            state,
            Checkpoint::Failed {
                at: Step::SyncingGuest,
                guest: GuestAfterFailure::Untouched,
                ..
            }
        ));
        assert_eq!(actions, [Action::SyncGuest]);
    }

    #[test]
    fn the_checkpoint_appears_under_its_label_only_at_the_commit() {
        let (_, actions) = run(
            AfterCheckpoint::Resume,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
            ],
        );
        let Some(Action::Files(commit)) = actions.last() else {
            panic!("{actions:?}")
        };
        assert_eq!(
            commit,
            &[
                FileOp::Flush {
                    path: "/d/machines/m/checkpoints/.staging/memory.vzsave".into()
                },
                FileOp::Rename {
                    from: "/d/machines/m/checkpoints/.staging".into(),
                    to: "/d/machines/m/checkpoints/cp".into()
                }
            ]
        );
        let earlier = &actions[..actions.len() - 1];
        assert!(
            !format!("{earlier:?}").contains("checkpoints/cp"),
            "nothing is written under the label before the commit"
        );
    }

    #[test]
    fn a_failed_commit_discards_the_staging_and_resumes() {
        let (state, actions) = run(
            AfterCheckpoint::Stop,
            vec![
                Ok(Event::GuestSynced(GuestSync::Synced)),
                Ok(Event::Paused),
                Ok(Event::FilesDone),
                Ok(Event::FilesDone),
                Ok(Event::MemorySaved { bytes: 1 }),
                Err(Unconfirmed::Failed("EIO".into())),
            ],
        );
        assert!(matches!(
            state,
            Checkpoint::Failed {
                at: Step::Committing,
                guest: GuestAfterFailure::ResumeRequested,
                ..
            }
        ));
        assert_eq!(
            &actions[actions.len() - 2..],
            [
                Action::Discard {
                    dir: "/d/machines/m/checkpoints/.staging".into()
                },
                Action::ResumeVm
            ]
        );
    }

    /// The VMM or the host may die at any point: a checkpoint under its label
    /// is then either absent or complete, never a partial memory file next to
    /// a disk.
    #[test]
    fn an_interrupted_checkpoint_is_absent_or_complete() {
        use crate::bundle::model::Disk;

        let bundle = DataRoot::new("/d".into())
            .unwrap()
            .machine(&"m".parse().unwrap());
        let label = "cp".parse().unwrap();
        let mut start = Disk::default();
        let image = bundle.image();
        start.write(image.spec.clone(), "spec");
        start.write(image.disk.clone(), "disk");
        start.write(image.aux.clone(), "aux");

        // Every file effect of a successful run, in order. Saving memory is
        // two effects: VZ creates the file, then finishes writing it.
        type Effect = Box<dyn Fn(&mut Disk)>;
        let mut effects: Vec<Effect> = Vec::new();
        let (mut state, mut queue) = Checkpoint::<String>::begin(CheckpointPlan::new(
            &bundle,
            &label,
            AfterCheckpoint::Stop,
        ));
        while let Some(action) = queue.pop() {
            let event = match action {
                Action::SyncGuest => Ok(Event::GuestSynced(GuestSync::Synced)),
                Action::PauseVm => Ok(Event::Paused),
                Action::ResumeVm => Ok(Event::Resumed),
                Action::StopVm => Ok(Event::Stopped),
                Action::Discard { .. } => panic!("a successful run discards nothing"),
                Action::Files(ops) => {
                    for op in ops {
                        effects.push(Box::new(move |d: &mut Disk| d.apply(&op).unwrap()));
                    }
                    Ok(Event::FilesDone)
                }
                Action::SaveMemory { path } => {
                    let partial = path.clone();
                    effects.push(Box::new(move |d: &mut Disk| {
                        d.write(partial.clone(), "partial")
                    }));
                    effects.push(Box::new(move |d: &mut Disk| {
                        d.write(path.clone(), "memory")
                    }));
                    Ok(Event::MemorySaved { bytes: 6 })
                }
            };
            let (next, more) = state.on(event).unwrap();
            state = next;
            queue.extend(more);
        }
        assert!(matches!(state, Checkpoint::Completed { .. }));

        let target = bundle.checkpoint(&label).dir;
        for crash in 0..=effects.len() {
            let mut disk = start.clone();
            for effect in &effects[..crash] {
                effect(&mut disk);
            }
            let leftovers = bundle.leftovers();
            disk.run(&leftovers.settle(&disk.present(&leftovers.probes())))
                .unwrap();
            if disk.exists(&target) {
                let files = disk.files_under(&target);
                assert_eq!(
                    files.into_iter().collect::<Vec<_>>(),
                    [
                        ("aux.bin", "aux"),
                        ("disk.img", "disk"),
                        ("memory.vzsave", "memory"),
                        ("spec.pb", "spec")
                    ]
                    .map(|(p, c)| (p.into(), c.to_owned())),
                    "crash {crash}"
                );
            }
            assert_eq!(
                disk.exists(&target),
                crash == effects.len(),
                "crash {crash}"
            );
            assert!(
                !disk.any_under(&bundle.checkpoint_staging().dir),
                "crash {crash}"
            );
        }
    }
}

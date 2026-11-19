//! `checkpoint`, `checkpoints`, `forget-checkpoint`, `restore`, `fork` and
//! `fork-run`: a machine's checkpoints and the machines made from them.

use std::ffi::OsString;

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use doppel_client::CheckpointResult;
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{AfterCheckpoint, GuestAfterFailure};
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::daemon_at::{LIFECYCLE, within};
use crate::fork_run;
use crate::report::Report;

/// Snapshot memory and disk of a running machine at one instant.
#[derive(Debug, Args)]
pub(crate) struct CheckpointCmd {
    name: MachineName,
    label: CheckpointLabel,
    #[arg(long, value_enum, default_value = "resume")]
    then: Then,
}

/// What a machine does once its checkpoint is taken.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Then {
    Resume,
    Stop,
}

impl CheckpointCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let Self { name, label, then } = self;
        let then = match then {
            Then::Resume => AfterCheckpoint::Resume,
            Then::Stop => AfterCheckpoint::Stop,
        };
        let report = ctx
            .daemon()?
            .connect()?
            .checkpoint(&name, &label, then, within(LIFECYCLE))?;
        for cleanup in &report.cleanup {
            let action = &cleanup.action;
            match &cleanup.result {
                Ok(()) => eprintln!("cleanup {action}: ok"),
                Err(e) => eprintln!("cleanup {action}: FAILED: {e}"),
            }
        }
        match report.result {
            CheckpointResult::Completed {
                memory_bytes,
                sync_skipped,
                guest,
            } => Ok(Report::Checkpointed {
                name,
                label,
                memory_bytes,
                paused_for: report.paused_for,
                sync_skipped,
                guest,
            }),
            CheckpointResult::Failed { at, error, guest } => {
                let guest = match guest {
                    GuestAfterFailure::Untouched => "guest was not paused",
                    GuestAfterFailure::ResumeRequested => "guest resume requested (see cleanup)",
                };
                bail!("checkpoint failed while {at:?}: {error}; {guest}");
            }
            CheckpointResult::Unknown { at, error } => bail!(
                "checkpoint outcome UNKNOWN: the VMM stopped answering while {at:?} ({error}). Check `doppel status {name}`"
            ),
            CheckpointResult::InProgress { awaiting } => bail!(
                "checkpoint outcome UNKNOWN: the procedure ended while {awaiting:?}, before it finished. Check `doppel status {name}`"
            ),
        }
    }
}

/// List a machine's checkpoints.
#[derive(Debug, Args)]
pub(crate) struct CheckpointsCmd {
    name: MachineName,
}

impl CheckpointsCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        Ok(Report::Checkpoints(
            ctx.machines().machine(&self.name).checkpoints()?,
        ))
    }
}

/// Delete one of a machine's checkpoints, for good. Refused for the
/// checkpoint a golden image's new machines start from.
#[derive(Debug, Args)]
pub(crate) struct ForgetCheckpointCmd {
    name: MachineName,
    label: CheckpointLabel,
}

impl ForgetCheckpointCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        ctx.daemon()?
            .connect()?
            .delete_checkpoint(&self.name, &self.label, within(LIFECYCLE))?;
        Ok(Report::CheckpointForgotten {
            name: self.name,
            label: self.label,
        })
    }
}

/// Create a new machine that resumes from a checkpoint of another.
#[derive(Debug, Args)]
pub(crate) struct ForkCmd {
    source: MachineName,
    label: CheckpointLabel,
    into: MachineName,
}

impl ForkCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        ctx.daemon()?
            .connect()?
            .fork(&self.source, &self.label, &self.into, within(LIFECYCLE))?;
        Ok(Report::Forked {
            source: self.source,
            label: self.label,
            into: self.into,
        })
    }
}

/// Fork a checkpoint fresh COUNT times, start the forks side by side,
/// and run the command once per fork with its name in `DOPPEL_MACHINE`
/// (which `doppel mcp` targets): `doppel fork-run dev clean --count 2 --
/// claude -p "…"`. The forks cold-boot from the checkpoint's disk with
/// identities of their own. When every command has ended, they are
/// stopped and kept.
#[derive(Debug, Args)]
pub(crate) struct ForkRunCmd {
    source: MachineName,
    label: CheckpointLabel,
    /// How many forks: at most the VMs macOS can run beside the
    /// running ones (two in all).
    #[arg(long, default_value_t = 2)]
    count: u32,
    /// Show the forks in `doppel view` while the commands run.
    #[arg(long)]
    view: bool,
    /// Have each command's `doppel mcp` record its session
    /// (`DOPPEL_RECORD=1`).
    #[arg(long)]
    record: bool,
    /// The program to run per fork, then its arguments.
    #[arg(required = true, last = true)]
    command: Vec<OsString>,
}

impl ForkRunCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        fork_run::ForkRun {
            source: self.source,
            label: self.label,
            count: self.count,
            command: self.command,
            watch: if self.view {
                fork_run::Watch::View
            } else {
                fork_run::Watch::Unwatched
            },
            recorded: if self.record {
                fork_run::Recorded::Recorded
            } else {
                fork_run::Recorded::Unrecorded
            },
            doppel: ctx.doppel_command()?,
        }
        .run(&ctx.daemon()?)?;
        Ok(Report::Nothing)
    }
}

/// Roll a stopped machine back to a checkpoint, discarding its current disk.
#[derive(Debug, Args)]
pub(crate) struct RestoreCmd {
    name: MachineName,
    label: CheckpointLabel,
    /// Required: the machine's current disk is replaced.
    #[arg(long, required = true)]
    discard_current: bool,
}

impl RestoreCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        ctx.daemon()?
            .connect()?
            .restore(&self.name, &self.label, within(LIFECYCLE))?;
        Ok(Report::Restored {
            name: self.name,
            label: self.label,
        })
    }
}

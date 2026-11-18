//! `golden`: golden images, the machines to fork from.

use anyhow::{Result, bail};
use clap::Subcommand;
use doppel_host::machines::{CHECKPOINT_TIMEOUT, CLAIM_TIMEOUT};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{AfterCheckpoint, Checkpoint, GuestSync};
use doppel_machine::machine::MachineName;
use doppel_protocol::transport::Cancel;

use crate::context::Context;
use crate::report::Report;

#[derive(Debug, Subcommand)]
pub(crate) enum GoldenCommand {
    /// Make a running, logged-in machine a golden image: no sleep, no
    /// screensaver, auto-login (using the stored password through sudo),
    /// then a checkpoint that powers it off. Forks of that checkpoint
    /// reach an agent-ready desktop on their own. The machine is then
    /// recorded as a golden image with that checkpoint as its template.
    Prepare {
        name: MachineName,
        /// The checkpoint to take.
        #[arg(long, default_value = "ready")]
        checkpoint: CheckpointLabel,
    },
    /// Record a machine as a golden image: a template to fork from, whose
    /// new machines start from one of its checkpoints. `ls` and the viewers
    /// show it apart from working machines.
    Mark {
        name: MachineName,
        /// The checkpoint new machines start from; it must exist.
        #[arg(long, default_value = "ready")]
        checkpoint: CheckpointLabel,
    },
    /// Record a golden image as a working machine again.
    Unmark { name: MachineName },
}

impl GoldenCommand {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        match self {
            Self::Prepare { name, checkpoint } => prepare(ctx, name, checkpoint),
            Self::Mark { name, checkpoint } => {
                let lineage = machines
                    .claim(&name, machines.within(CLAIM_TIMEOUT))?
                    .mark_golden(&checkpoint)?;
                Ok(Report::GoldenMarked {
                    name,
                    label: checkpoint,
                    lineage,
                })
            }
            Self::Unmark { name } => {
                machines
                    .claim(&name, machines.within(CLAIM_TIMEOUT))?
                    .mark_working()?;
                Ok(Report::GoldenUnmarked(name))
            }
        }
    }
}

pub(crate) fn prepare(ctx: &Context, name: MachineName, label: CheckpointLabel) -> Result<Report> {
    let machines = ctx.machines();
    // Owned from the first setting to the checkpoint: nothing else may
    // change the guest in between.
    let claim = machines.claim(&name, machines.within(CLAIM_TIMEOUT))?;
    let ready = machines.machine(&name).await_desktop(
        doppel_machine::login::LoginPolicy::DEFAULT,
        &Cancel::never(),
    )?;
    if !ready.is_ready() {
        bail!("{name}: not ready: {ready}");
    }
    machines
        .machine(&name)
        .golden_prepare(|step| eprintln!("{name}: {step}…"))?;
    eprintln!("{name}: settings applied; checkpointing as '{label}' and stopping…");
    let report = claim.checkpoint(
        &label,
        AfterCheckpoint::Stop,
        machines.within(CHECKPOINT_TIMEOUT),
    )?;
    match report.result {
        Checkpoint::Completed {
            guest_sync: GuestSync::Skipped(reason),
            ..
        } => bail!(
            "{name}: checkpoint '{label}' was saved without syncing the guest ({reason}): its disk may lack the auto-login settings, so cold boots from it can stop at the login window. Don't fork it; prepare again under a new label"
        ),
        Checkpoint::Completed {
            memory_bytes,
            guest_sync: GuestSync::Synced,
            ..
        } => {
            claim.mark_golden(&label)?;
            Ok(Report::GoldenPrepared {
                name,
                label,
                memory_bytes,
            })
        }
        other @ (Checkpoint::InProgress { .. }
        | Checkpoint::Failed { .. }
        | Checkpoint::Unknown { .. }) => {
            bail!("{name}: checkpoint '{label}' didn't complete: {other:?}")
        }
    }
}

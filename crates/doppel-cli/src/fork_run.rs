//! `doppel fork-run`: fresh forks of a checkpoint, started side by side by
//! the daemon, each handed to its own run of a command (an agent) that finds
//! its machine in `DOPPEL_MACHINE`. When every run has ended, the forks are
//! stopped and kept, to look at or to fork again.
//!
//! Interrupted (Ctrl-C), the commands end with it and the forks keep
//! running: `doppel stop` them.

use std::ffi::OsString;
use std::process::{Child, Command, ExitStatus};

use anyhow::{Context, Result, bail};
use doppel_client::{MachineFailure, StopMode, StopOutcome};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::login::Desktop;
use doppel_machine::machine::MachineName;

use crate::daemon_at::{DaemonAt, LIFECYCLE, within};
use doppel_host::machines::CALL_TIMEOUT;

/// Whether the forks are shown while they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watch {
    /// In `doppel view`, closed when the run ends.
    View,
    Unwatched,
}

/// Whether each command's `doppel mcp` records its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recorded {
    /// `DOPPEL_RECORD=1` in the command's environment.
    Recorded,
    Unrecorded,
}

/// One `doppel fork-run`.
#[derive(Debug)]
pub(crate) struct ForkRun {
    pub(crate) source: MachineName,
    pub(crate) label: CheckpointLabel,
    pub(crate) count: u32,
    /// The program and its arguments, run once per fork.
    pub(crate) command: Vec<OsString>,
    pub(crate) watch: Watch,
    pub(crate) recorded: Recorded,
    /// `doppel` and the global flags this one was given, for `doppel view`.
    pub(crate) doppel: Vec<OsString>,
}

/// How one fork's run ended.
#[derive(Debug)]
enum Ended {
    /// It never started, so its command didn't run.
    NotStarted(MachineFailure),
    /// It started without reaching an agent-ready desktop (locked, say), so
    /// its command didn't run.
    NotReady(Desktop),
    /// Its command couldn't be started or waited for.
    NotRun(std::io::Error),
    Exited(ExitStatus),
}

/// A fork's command, once the fork is up.
#[derive(Debug)]
enum Agent {
    Running(Child),
    Ended(Ended),
}

/// A fork's run and how stopping it went.
#[derive(Debug)]
struct Report {
    name: MachineName,
    ended: Ended,
    stopped: Result<(), String>,
}

impl Report {
    fn succeeded(&self) -> bool {
        let ran = match &self.ended {
            Ended::Exited(status) => status.success(),
            Ended::NotStarted(_) | Ended::NotReady(_) | Ended::NotRun(_) => false,
        };
        ran && self.stopped.is_ok()
    }

    fn line(&self) -> String {
        let ended = match &self.ended {
            Ended::NotStarted(failure) => format!("didn't start: {failure}"),
            Ended::NotReady(outcome) => format!("no desktop for the command: {outcome}"),
            Ended::NotRun(error) => format!("the command couldn't run: {error}"),
            Ended::Exited(status) => match status.code() {
                Some(code) => format!("the command exited with {code}"),
                None => format!("the command ended: {status}"),
            },
        };
        let stopped = match &self.stopped {
            Ok(()) => "stopped and kept".to_owned(),
            Err(error) => format!("not stopped: {error}"),
        };
        format!("{}: {ended}; {stopped}", self.name)
    }
}

impl ForkRun {
    /// Runs the forks to the end. Fails when a fork or its command didn't
    /// succeed, after reporting every one.
    pub(crate) fn run(self, at: &DaemonAt) -> Result<()> {
        let Some((program, args)) = self.command.split_first() else {
            bail!("give the command to run on each fork after `--`");
        };
        let forks =
            at.connect()?
                .run_forks(&self.source, &self.label, self.count, within(LIFECYCLE))?;
        let mut ready = Vec::new();
        for fork in &forks {
            match &fork.desktop {
                Ok(outcome) if outcome.is_ready() => {
                    eprintln!("{}: ready ({outcome})", fork.name);
                    ready.push(&fork.name);
                }
                Ok(outcome) => eprintln!("{}: not ready: {outcome}", fork.name),
                Err(failure) => eprintln!("{}: didn't start: {failure}", fork.name),
            }
        }
        let console = match self.watch {
            Watch::View if !ready.is_empty() => Some(self.console(&ready)?),
            Watch::View | Watch::Unwatched => None,
        };

        let agents: Vec<(MachineName, Agent)> = forks
            .into_iter()
            .map(|fork| {
                let agent = match fork.desktop {
                    Err(failure) => Agent::Ended(Ended::NotStarted(failure)),
                    Ok(outcome) if !outcome.is_ready() => Agent::Ended(Ended::NotReady(outcome)),
                    Ok(_) => {
                        let mut command = Command::new(program);
                        command.args(args).env("DOPPEL_MACHINE", fork.name.as_str());
                        match self.recorded {
                            Recorded::Recorded => command.env("DOPPEL_RECORD", "1"),
                            Recorded::Unrecorded => command.env_remove("DOPPEL_RECORD"),
                        };
                        match command.spawn() {
                            Ok(child) => Agent::Running(child),
                            Err(error) => Agent::Ended(Ended::NotRun(error)),
                        }
                    }
                };
                (fork.name, agent)
            })
            .collect();
        let reports: Vec<Report> = agents
            .into_iter()
            .map(|(name, agent)| {
                let ended = match agent {
                    Agent::Running(mut child) => match child.wait() {
                        Ok(status) => Ended::Exited(status),
                        Err(error) => Ended::NotRun(error),
                    },
                    Agent::Ended(ended) => ended,
                };
                // Every fork is stopped, even one whose start failed: it
                // may be up anyway. One without a desktop is powered off: a
                // shutdown from its session can't be asked for, and the
                // power button doesn't stop a Mac at its login window or
                // setup screens.
                let mode = match ended {
                    Ended::NotStarted(_) | Ended::NotReady(_) => StopMode::Force,
                    Ended::NotRun(_) | Ended::Exited(_) => StopMode::Graceful,
                };
                let stopped = Self::stop(at, &name, mode);
                Report {
                    name,
                    ended,
                    stopped,
                }
            })
            .collect();
        if let Some(mut console) = console {
            // The window would only show stopped machines now.
            let _ = console.kill();
            let _ = console.wait();
        }
        for report in &reports {
            println!("{}", report.line());
        }
        let failed = reports.iter().filter(|report| !report.succeeded()).count();
        if failed > 0 {
            bail!("{failed} of {} forks didn't run to success", reports.len());
        }
        Ok(())
    }

    fn console(&self, names: &[&MachineName]) -> Result<Child> {
        let Some((doppel, globals)) = self.doppel.split_first() else {
            bail!("no doppel to run the console with");
        };
        Command::new(doppel)
            .args(globals)
            .arg("view")
            .args(names.iter().map(|name| name.as_str()))
            .spawn()
            .context("opening the console")
    }

    /// Stops a fork as `mode` says, on a connection of its own. A fork
    /// that isn't running counts as stopped.
    fn stop(at: &DaemonAt, name: &MachineName, mode: StopMode) -> Result<(), String> {
        let stop = || -> Result<()> {
            let client = at.connect()?;
            match client.status(name, within(CALL_TIMEOUT))? {
                doppel_machine::vm::Status::Stopped(_) => Ok(()),
                doppel_machine::vm::Status::Live(_) => {
                    match client.stop(name, mode, within(LIFECYCLE))?.outcome {
                        StopOutcome::Stopped => Ok(()),
                        StopOutcome::ShutdownPending => {
                            bail!("its shutdown is still pending; check a screenshot")
                        }
                    }
                }
            }
        };
        stop().map_err(|error| format!("{error:#}"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;

    use doppel_desktop::outcome::Certainty;

    use super::*;

    fn report(ended: Ended, stopped: Result<(), String>) -> Report {
        Report {
            name: "dev-clean-1".parse().unwrap(),
            ended,
            stopped,
        }
    }

    #[test]
    fn a_fork_succeeds_only_if_its_command_did_and_it_stopped() {
        let cases = [
            (report(Ended::Exited(ExitStatus::from_raw(0)), Ok(())), true),
            (
                report(Ended::Exited(ExitStatus::from_raw(0)), Err("busy".into())),
                false,
            ),
            // Exit code 3, as a wait status.
            (
                report(Ended::Exited(ExitStatus::from_raw(3 << 8)), Ok(())),
                false,
            ),
            (
                report(
                    Ended::NotStarted(MachineFailure::new("no VMM".into(), Certainty::NotDone)),
                    Ok(()),
                ),
                false,
            ),
            (
                report(
                    Ended::NotReady(Desktop {
                        login: doppel_machine::login::LoginOutcome::NeedsPassword,
                        screen: None,
                    }),
                    Ok(()),
                ),
                false,
            ),
        ];
        for (report, succeeded) in cases {
            assert_eq!(report.succeeded(), succeeded, "{}", report.line());
        }
    }
}

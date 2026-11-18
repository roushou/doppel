//! `agent`: installing the guest agent.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Subcommand;
use doppel_host::agent_grant::plan::CodeRequirement;
use doppel_host::bootstrap::AgentArchive;
use doppel_host::capabilities::{SystemClock, WallClock as _};
use doppel_host::golden::plan::GuestUserName;
use doppel_host::machines::CLAIM_TIMEOUT;
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::report::Report;

#[derive(Debug, Subcommand)]
pub(crate) enum AgentCommand {
    /// Install `Doppel Agent.app` in the guest as a LaunchAgent (started at
    /// login, restarted if it exits), through the agent running there now.
    Install {
        name: MachineName,
        /// The zipped app from scripts/build.sh. Defaults to the one next to `doppel`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Install the agent in a machine that has none yet: log in over SSH
    /// with the stored password, start the app, then install it as `install`
    /// does. For a machine made with `doppel create … --user`, after its
    /// first boot. With an agent already answering, only installs.
    Bootstrap {
        name: MachineName,
        /// The guest account to log in as: `create --user`'s.
        #[arg(long)]
        user: GuestUserName,
        /// The zipped app from scripts/build.sh. Defaults to the one next to `doppel`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
    /// Grant the agent Accessibility and Screen Recording in a stopped
    /// machine, so it boots with them and no person answers a prompt. Writes
    /// the guest's own privacy records into its disk, for the agent build
    /// beside `doppel` (its code requirement, `<archive>.csreq`). The machine
    /// must be shut down (`doppel stop`), not paused or saved. After
    /// `bootstrap`: stop, grant, start.
    Grant {
        name: MachineName,
        /// The guest account the agent runs as: `create --user`'s.
        #[arg(long)]
        user: GuestUserName,
        /// The zipped app from scripts/build.sh. Defaults to the one next to `doppel`.
        #[arg(long)]
        archive: Option<PathBuf>,
    },
}

impl AgentCommand {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        match self {
            Self::Install { name, archive } => {
                let archive = agent_archive(archive)?;
                let bytes = std::fs::read(&archive.path)
                    .with_context(|| archive.path.display().to_string())?;
                let installed =
                    machines
                        .machine(&name)
                        .install_agent(bytes, archive.build, |step| {
                            eprintln!("{name}: {step}…");
                        })?;
                Ok(Report::AgentInstalled {
                    name,
                    over_ssh: None,
                    installed,
                })
            }
            Self::Grant {
                name,
                user,
                archive,
            } => {
                let path = match archive {
                    Some(path) => path,
                    None => std::env::current_exe()?.with_file_name("doppel-agent.zip"),
                };
                let csreq = path.with_extension("csreq");
                let requirement =
                    CodeRequirement::parse(std::fs::read(&csreq).with_context(|| {
                        format!("{} (scripts/build.sh writes it)", csreq.display())
                    })?)
                    .with_context(|| csreq.display().to_string())?;
                machines
                    .claim(&name, machines.within(CLAIM_TIMEOUT))?
                    .grant_agent(&user, &requirement, SystemClock.wall(), machines.deadline())?;
                Ok(Report::AgentGranted { name, user })
            }
            Self::Bootstrap {
                name,
                user,
                archive,
            } => {
                use doppel_host::bootstrap::Started;
                let archive = agent_archive(archive)?;
                let done = machines.machine(&name).bootstrap_agent(
                    user,
                    &archive,
                    &std::env::current_exe()?,
                    |progress| eprintln!("{name}: {progress}…"),
                )?;
                if let Some((dir, error)) = &done.leftover {
                    eprintln!("{name}: warning: couldn't remove {dir} in the guest: {error}");
                }
                Ok(Report::AgentInstalled {
                    name,
                    over_ssh: match done.started {
                        Started::AlreadyRunning => None,
                        Started::OverSsh(address) => Some(address),
                    },
                    installed: done.installed,
                })
            }
        }
    }
}

/// The zipped agent app and its build id, beside it as `<archive>.build-id`.
pub(crate) fn agent_archive(archive: Option<PathBuf>) -> Result<AgentArchive> {
    let path = match archive {
        Some(path) => path,
        None => std::env::current_exe()?.with_file_name("doppel-agent.zip"),
    };
    let build_file = path.with_extension("build-id");
    let build = std::fs::read_to_string(&build_file)
        .with_context(|| build_file.display().to_string())?
        .trim()
        .parse()?;
    Ok(AgentArchive { path, build })
}

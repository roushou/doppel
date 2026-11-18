//! `doctor`: gathers what [`Diagnosis`] judges.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use anyhow::Result;
use clap::Args;
use doppel_client::{Client, ClientError, Listing};
use doppel_host::capabilities::SystemClock;
use doppel_host::machines::CALL_TIMEOUT;
use doppel_machine::lineage::{RecordedLineage, Role as LineageRole};
use doppel_machine::vm::Status;

use crate::commands::agent::agent_archive;
use crate::context::{Context, InstallDir};
use crate::daemon_at::within;
use crate::doctor::{
    Archive, Checkpoint, Daemon, Diagnosis, Facts, Machine, Power, Program, Role, Service,
};
use crate::launchd::Installation;
use crate::report::{Form, Report};

/// Check that an agent can be given a machine here (`doppel mcp --from
/// GOLDEN`): the VMM, the guest agent's archive, the daemon, a golden
/// image to fork, and room to run it. Says what to do about each problem,
/// and exits 1 unless ready.
#[derive(Debug, Args)]
pub(crate) struct DoctorCmd {
    /// Print one JSON document, for programs: `ready`, the
    /// `golden_images` to pass to `--from`, and each check's `status`,
    /// `detail` and `fix`.
    #[arg(long)]
    json: bool,
}

impl DoctorCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let agent_archive = InstallDir::current()?.agent_archive();
        let facts = Facts {
            vmm: ctx.vmm().to_path_buf(),
            vmm_found: program(ctx.vmm()),
            agent_archive_found: archive(&agent_archive),
            agent_archive,
            daemon: daemon(ctx),
            max_running: doppel_daemon::MAX_RUNNING_VMS,
        };
        Ok(Report::Doctor {
            diagnosis: Diagnosis::of(&facts),
            form: if self.json { Form::Json } else { Form::Text },
        })
    }
}

fn program(path: &Path) -> Program {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => Program::Present,
        Ok(_) => Program::NotExecutable,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Program::Missing,
        Err(error) => Program::Unreadable(error.to_string()),
    }
}

fn archive(path: &Path) -> Archive {
    if !path.exists() {
        return Archive::Missing;
    }
    match agent_archive(Some(path.to_path_buf())) {
        Ok(archive) => Archive::Present(archive.build.to_string()),
        Err(error) => Archive::Unreadable(crate::ErrorMessage::of(&error).to_string()),
    }
}

/// Connecting starts the daemon when launchd has it: as any client would.
fn daemon(ctx: &Context) -> Daemon {
    let socket = match ctx.daemon() {
        Ok(at) => at.socket().to_path_buf(),
        Err(error) => return Daemon::Failed(crate::ErrorMessage::of(&error).to_string()),
    };
    match Client::connect(&socket, SystemClock, within(CALL_TIMEOUT)) {
        Ok(client) => match machines(&client) {
            Ok(machines) => Daemon::Answered(machines),
            Err(error) => Daemon::Failed(error.to_string()),
        },
        Err(ClientError::NoDaemon(_)) => {
            Daemon::NotRunning(match ctx.service().map(|service| service.installation()) {
                Ok(Installation::This) => Service::Installed,
                Ok(Installation::Another) => Service::InstalledForAnother,
                Ok(Installation::None) | Err(_) => Service::NotInstalled,
            })
        }
        Err(ClientError::VersionMismatch { ours, theirs }) => Daemon::OtherVersion { ours, theirs },
        Err(error) => Daemon::Failed(error.to_string()),
    }
}

fn machines(client: &Client<SystemClock>) -> Result<Vec<Machine>, ClientError> {
    let mut machines = Vec::new();
    for listing in client.machines(within(CALL_TIMEOUT))? {
        let Listing::Machine {
            name,
            status,
            lineage,
        } = listing
        else {
            continue;
        };
        // As the daemon counts them against Apple's limit.
        let power = match status {
            Ok(Status::Live(_)) => Power::Live,
            Ok(Status::Stopped(_)) | Err(_) => Power::Off,
        };
        let role = match lineage {
            RecordedLineage::Unreadable(why) => Role::Unreadable(why),
            RecordedLineage::Known(lineage) => match lineage.role {
                LineageRole::Golden { template } => {
                    let template_found = match client.report(&name, within(CALL_TIMEOUT)) {
                        Ok(report) if report.checkpoints.contains(&template) => Checkpoint::Present,
                        Ok(_) => Checkpoint::Missing,
                        Err(error) => Checkpoint::Unknown(error.to_string()),
                    };
                    Role::Golden {
                        template,
                        template_found,
                    }
                }
                LineageRole::Working | LineageRole::Temporary => Role::Other,
            },
        };
        machines.push(Machine { name, power, role });
    }
    Ok(machines)
}

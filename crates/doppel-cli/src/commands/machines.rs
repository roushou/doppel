//! `ls`, `create`, `import`, `status` and `delete`: which machines there
//! are, and making and removing them.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Args;
use doppel_host::capabilities::OsEntropy;
use doppel_host::golden::plan::GuestUserName;
use doppel_host::machines::{CALL_TIMEOUT, CLAIM_TIMEOUT, ImportSource};
use doppel_host::provision::FullName;
use doppel_machine::machine::{
    Access, CpuCount, DiskSize, DisplaySpec, Hardware, MacAddress, MachineName, MemorySize,
    NetworkSpec, ShareName, SharedDirectory,
};

use crate::context::Context;
use crate::daemon_at::{LIFECYCLE, within};
use crate::report::Report;

/// List machines.
#[derive(Debug, Args)]
pub(crate) struct LsCmd {}

impl LsCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        Ok(Report::Machines(
            ctx.daemon()?.connect()?.machines(within(CALL_TIMEOUT))?,
        ))
    }
}

/// Install macOS into a new machine from a restore image.
///
/// With `--user`, macOS creates that account itself on the first boot
/// (macOS 27 hosts and restore images): logged in automatically, Remote
/// Login on, and a generated password stored as the machine's login
/// password. The machine is left running. Without it, a person goes
/// through Setup Assistant (`doppel view`).
#[derive(Debug, Args)]
pub(crate) struct CreateCmd {
    name: MachineName,
    #[arg(long)]
    ipsw: PathBuf,
    /// The account's short name.
    #[arg(long)]
    user: Option<GuestUserName>,
    /// The account's full name. Defaults to the short name.
    #[arg(long, requires = "user")]
    full_name: Option<FullName>,
    #[command(flatten)]
    hardware: HardwareArgs,
}

impl CreateCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        use doppel_host::provision::{Account, Setup};
        use doppel_machine::login::LoginPassword;
        let Self {
            name,
            ipsw,
            user,
            full_name,
            hardware,
        } = self;
        let machines = ctx.machines();
        let setup = match user {
            None => Setup::Assistant,
            Some(user) => Setup::Provision(Account {
                full_name: match full_name {
                    Some(full_name) => full_name,
                    None => user.as_str().parse()?,
                },
                user,
                password: LoginPassword::generate(&mut OsEntropy)
                    .context("generating a password")?,
            }),
        };
        let claim = machines.claim(&name, machines.within(CLAIM_TIMEOUT))?;
        let mut last = -1i64;
        let created = claim.create(hardware.parse()?, &ipsw, &setup, |fraction| {
            let pct = (fraction * 100.0) as i64;
            if pct / 5 != last / 5 {
                last = pct;
                eprintln!("installing macOS: {pct}%");
            }
        })?;
        Ok(Report::Created {
            name,
            state: created,
            account: match setup {
                Setup::Provision(account) => Some(account.user),
                Setup::Assistant => None,
            },
        })
    }
}

/// Adopt existing VZ machine files (cloned, not moved).
#[derive(Debug, Args)]
pub(crate) struct ImportCmd {
    name: MachineName,
    #[arg(long)]
    hardware_model: PathBuf,
    #[arg(long)]
    machine_identifier: PathBuf,
    #[arg(long)]
    aux: PathBuf,
    #[arg(long)]
    disk: PathBuf,
    #[command(flatten)]
    hardware: HardwareArgs,
}

impl ImportCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        machines
            .claim(&self.name, machines.within(CLAIM_TIMEOUT))?
            .import(
                self.hardware.parse()?,
                &ImportSource {
                    hardware_model: self.hardware_model,
                    machine_identifier: self.machine_identifier,
                    aux: self.aux,
                    disk: self.disk,
                },
            )?;
        Ok(Report::Imported(self.name))
    }
}

#[derive(Debug, Args)]
pub(crate) struct StatusCmd {
    name: MachineName,
}

impl StatusCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let status = ctx
            .daemon()?
            .connect()?
            .status(&self.name, within(CALL_TIMEOUT))?;
        Ok(Report::Status {
            name: self.name,
            status,
        })
    }
}

/// Delete a machine for good: its disk and its checkpoints. Its
/// recordings stay. It must be stopped, unless `--stop`. Asks first in a
/// terminal; anywhere else, pass `--yes`.
#[derive(Debug, Args)]
pub(crate) struct DeleteCmd {
    name: MachineName,
    /// Don't ask: for scripts.
    #[arg(long)]
    yes: bool,
    /// Power it off first if it runs (as `stop --force`).
    #[arg(long)]
    stop: bool,
}

impl DeleteCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        use doppel_client::{Listing, StopMode};
        use doppel_machine::lineage::{RecordedLineage, Role};
        use doppel_machine::vm::Status;
        let client = ctx.daemon()?.connect()?;
        let name = self.name;
        let report = client.report(&name, within(CALL_TIMEOUT))?;
        let golden =
            client
                .machines(within(CALL_TIMEOUT))?
                .into_iter()
                .any(|listing| match listing {
                    Listing::Machine {
                        name: listed,
                        lineage: RecordedLineage::Known(lineage),
                        ..
                    } => listed == name && matches!(lineage.role, Role::Golden { .. }),
                    Listing::Machine { .. } | Listing::Unrecognized(_) => false,
                });
        let running = match report.status {
            Status::Live(_) => true,
            Status::Stopped(_) => false,
        };
        if running && !self.stop {
            bail!("{name} is running: stop it first (doppel stop {name}), or pass --stop");
        }
        if !self.yes {
            let what = format!(
                "{}{name}{}{}",
                if golden { "golden image " } else { "" },
                match report.checkpoints.len() {
                    0 => String::new(),
                    1 => " and its checkpoint".to_owned(),
                    n => format!(" and its {n} checkpoints"),
                },
                if running { ", powering it off," } else { "" },
            );
            if !confirm(&format!("Delete {what} for good? [y/N] "))? {
                bail!("{name}: not deleted");
            }
        }
        if running {
            client.stop(&name, StopMode::Force, within(LIFECYCLE))?;
        }
        client.delete(&name, within(LIFECYCLE))?;
        Ok(Report::Deleted(name))
    }
}

/// Asks on the terminal; anything but yes is no. Not a terminal: refused,
/// so a script must say `--yes`.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{BufRead as _, IsTerminal as _};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("deleting is for good: pass --yes to do it without asking");
    }
    eprint!("{question}");
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// A new machine's hardware, as `create` and `import` take it.
#[derive(Debug, Args)]
pub(crate) struct HardwareArgs {
    #[arg(long, default_value_t = 4)]
    cpus: u32,
    #[arg(long, default_value_t = 4)]
    memory_gib: u32,
    #[arg(long, default_value_t = 64)]
    disk_gib: u32,
    /// Display width in points (pixels are 2×, Retina).
    #[arg(long, default_value_t = 1280)]
    width: u32,
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Fixed MAC address. Generated if omitted.
    #[arg(long, conflicts_with = "offline")]
    mac: Option<MacAddress>,
    /// No network device.
    #[arg(long)]
    offline: bool,
    /// Share a host directory read-only: NAME=/absolute/path. Repeatable.
    #[arg(long = "share", value_name = "NAME=PATH")]
    shares: Vec<ShareArg>,
    /// Share a host directory read-write: NAME=/absolute/path. Repeatable.
    #[arg(long = "share-rw", value_name = "NAME=PATH")]
    shares_rw: Vec<ShareArg>,
}

impl HardwareArgs {
    pub(crate) fn parse(self) -> Result<Hardware> {
        let network = if self.offline {
            NetworkSpec::None
        } else {
            let mac = match self.mac {
                Some(mac) => mac,
                None => MacAddress::generate(&mut OsEntropy).context("generating a MAC address")?,
            };
            NetworkSpec::Nat { mac }
        };
        let share = |share: &ShareArg, access| -> Result<SharedDirectory> {
            Ok(SharedDirectory::new(
                share.name.clone(),
                share.path.clone(),
                access,
            )?)
        };
        let shares = self
            .shares
            .iter()
            .map(|s| share(s, Access::ReadOnly))
            .chain(self.shares_rw.iter().map(|s| share(s, Access::ReadWrite)))
            .collect::<Result<_>>()?;
        Ok(Hardware {
            cpus: CpuCount::new(self.cpus)?,
            memory: MemorySize::from_gib(self.memory_gib)?,
            disk: DiskSize::from_gib(self.disk_gib)?,
            display: DisplaySpec::retina(self.width, self.height)?,
            network,
            shares,
        })
    }
}

/// A host directory to share, as `--share` takes it: `NAME=PATH`.
#[derive(Debug, Clone)]
struct ShareArg {
    name: ShareName,
    path: PathBuf,
}

impl std::str::FromStr for ShareArg {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        let (name, path) = raw
            .split_once('=')
            .with_context(|| format!("expected NAME=PATH, got {raw:?}"))?;
        Ok(Self {
            name: name.parse()?,
            path: PathBuf::from(path),
        })
    }
}

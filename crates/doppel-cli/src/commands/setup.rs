//! `setup`: gathers what [`Next`] decides on, and runs each step it names
//! until the golden image is ready or setup is blocked.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use clap::Args;
use doppel_client::{StopMode, Window};
use doppel_host::capabilities::OsEntropy;
use doppel_host::golden::plan::GuestUserName;
use doppel_host::launcher::VmmBinary;
use doppel_host::machines::CLAIM_TIMEOUT;
use doppel_host::provision::{Account, Setup};
use doppel_host::restore_image::plan::Downloaded;
use doppel_machine::login::LoginPassword;
use doppel_machine::machine::{Hardware, MachineName};

use crate::commands::agent::{agent_archive, grant};
use crate::commands::doctor::daemon;
use crate::commands::golden::prepare;
use crate::commands::lifecycle::await_desktop;
use crate::commands::machines::HardwareArgs;
use crate::context::Context;
use crate::daemon_at::{LIFECYCLE, within};
use crate::report::{Form, Report, SetupOutcome};
use crate::setup::{Facts, Image, Next, Phase, Record, Step};

/// More steps than setup ever takes: a run that gets here is going round.
const MAX_STEPS: usize = 16;

/// Make this Mac ready to give agents machines of their own, from nothing:
/// install the daemon, download the newest macOS this Mac supports, install
/// it into a new machine with its account made, install and grant the
/// guest agent, and make the machine a golden image. Takes a while (macOS
/// is a large download and install); prints each step on stderr. Run it
/// again after an interruption or a fix: it carries on where it stopped.
#[derive(Debug, Args)]
pub(crate) struct SetupCmd {
    /// The golden image to make.
    #[arg(long, default_value = "golden")]
    name: MachineName,
    /// The guest account's short name.
    #[arg(long, default_value = "doppel")]
    user: GuestUserName,
    /// Install from this restore image instead of downloading the newest.
    #[arg(long)]
    ipsw: Option<PathBuf>,
    /// Print one JSON document, for programs: `ready`, then the
    /// `golden_image` and its `mcp_command`, or why setup is `blocked`.
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    hardware: HardwareArgs,
}

impl SetupCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let form = if self.json { Form::Json } else { Form::Text };
        let record_path = ctx.machines().store().root().setup_record();
        let host = host_macos()?;
        let mut image = match self.ipsw {
            Some(path) => Image::Given(std::path::absolute(path)?),
            None => Image::None,
        };
        let mut hardware = Some(self.hardware);
        let mut last = None;
        for _ in 0..MAX_STEPS {
            let record = read_record(&record_path)?;
            let facts = Facts {
                host,
                daemon: daemon(ctx),
                name: self.name.clone(),
                record: record.clone(),
                image: image.clone(),
            };
            let step = match Next::of(&facts) {
                Next::Run(step) => step,
                Next::Done(template) => {
                    remove_record(&record_path, &self.name)?;
                    return Ok(Report::Setup {
                        outcome: SetupOutcome::Ready {
                            golden: self.name,
                            template,
                        },
                        form,
                    });
                }
                Next::Blocked(blocked) => {
                    return Ok(Report::Setup {
                        outcome: SetupOutcome::Blocked(blocked),
                        form,
                    });
                }
            };
            if last.as_ref() == Some(&step) {
                bail!("{}: {step} didn't take; see `doppel doctor`", self.name);
            }
            eprintln!("{}: {step}…", self.name);
            // The record's user, once there is one: the account is made then.
            let user = record
                .filter(|record| record.machine == self.name)
                .map_or_else(|| self.user.clone(), |record| record.user);
            let run = Run {
                ctx,
                name: &self.name,
                user: &user,
                record: &record_path,
            };
            match &step {
                Step::InstallDaemon => ctx.service()?.install()?,
                Step::RestartDaemon => {
                    ctx.service()?.stop()?;
                }
                Step::Download => image = Image::Downloaded(run.download()?),
                Step::Create(ipsw) => {
                    let hardware = hardware
                        .take()
                        .context("installing macOS a second time")?
                        .parse()?;
                    run.create(hardware, ipsw)?;
                    if let Image::Downloaded(path) = &image {
                        // Installed: the image is no use to keep.
                        std::fs::remove_file(path)
                            .with_context(|| format!("removing {}", path.display()))?;
                    }
                    image = Image::None;
                }
                Step::Boot => {
                    ctx.daemon()?.connect()?.start(
                        &self.name,
                        Window::Hidden,
                        within(LIFECYCLE),
                    )?;
                }
                Step::Bootstrap => run.bootstrap()?,
                Step::Stop => {
                    ctx.daemon()?.connect()?.stop(
                        &self.name,
                        StopMode::Graceful,
                        within(LIFECYCLE),
                    )?;
                }
                Step::Grant => {
                    grant(ctx, &self.name, &user, None)?;
                    run.reached(Phase::Granted)?;
                }
                Step::Start => {
                    let client = ctx.daemon()?.connect()?;
                    client.start(&self.name, Window::Hidden, within(LIFECYCLE))?;
                    eprint!("{}", await_desktop(&client, self.name.clone())?);
                }
                Step::Prepare => {
                    eprint!("{}", prepare(ctx, self.name.clone(), "ready".parse()?)?);
                }
            }
            last = Some(step);
        }
        bail!("{}: setup took more than {MAX_STEPS} steps", self.name)
    }
}

/// One setup run's machine, and where its record goes.
struct Run<'a> {
    ctx: &'a Context,
    name: &'a MachineName,
    user: &'a GuestUserName,
    record: &'a Path,
}

impl Run<'_> {
    /// Records `phase` as finished.
    fn reached(&self, phase: Phase) -> Result<()> {
        let record = Record {
            machine: self.name.clone(),
            user: self.user.clone(),
            phase,
        };
        std::fs::write(self.record, record.to_json())
            .with_context(|| self.record.display().to_string())
    }

    /// The newest restore image, downloaded into the store.
    fn download(&self) -> Result<PathBuf> {
        let latest = VmmBinary::new(self.ctx.vmm().to_path_buf()).latest_image()?;
        let dir = self.ctx.machines().store().root().restore_images_dir();
        eprintln!("{}: macOS {} from {}", self.name, latest.build, latest.url);
        let mut said = None;
        let path = latest.download(&dir, |done: Downloaded| {
            // Each percent, or each GB when the size isn't known.
            let mark = done
                .fraction()
                .map_or(done.bytes / 1_000_000_000, |share| (share * 100.0) as u64);
            if said != Some(mark) {
                said = Some(mark);
                eprintln!("{}: downloaded {done}", self.name);
            }
        })?;
        Ok(path)
    }

    /// Installs macOS with the account made on its first boot, which
    /// leaves it running.
    fn create(&self, hardware: Hardware, ipsw: &Path) -> Result<()> {
        let machines = self.ctx.machines();
        let setup = Setup::Provision(Account {
            full_name: self.user.as_str().parse()?,
            user: self.user.clone(),
            password: LoginPassword::generate(&mut OsEntropy).context("generating a password")?,
        });
        let claim = machines.claim(self.name, machines.within(CLAIM_TIMEOUT))?;
        self.reached(Phase::Creating)?;
        let mut said = -1i64;
        claim.create(hardware, ipsw, &setup, |fraction| {
            let pct = (fraction * 100.0) as i64;
            if pct / 5 != said / 5 {
                said = pct;
                eprintln!("{}: installing macOS: {pct}%", self.name);
            }
        })?;
        self.reached(Phase::Created)
    }

    /// Installs the agent over SSH, as `agent bootstrap` does.
    fn bootstrap(&self) -> Result<()> {
        let done = self.ctx.machines().machine(self.name).bootstrap_agent(
            self.user.clone(),
            &agent_archive(None)?,
            &std::env::current_exe()?,
            |progress| eprintln!("{}: {progress}…", self.name),
        )?;
        if let Some((dir, error)) = &done.leftover {
            eprintln!(
                "{}: warning: couldn't remove {dir} in the guest: {error}",
                self.name
            );
        }
        self.reached(Phase::Bootstrapped)
    }
}

/// This Mac's macOS major version.
fn host_macos() -> Result<crate::setup::MacOsMajor> {
    let output = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .context("running sw_vers")?;
    Ok(String::from_utf8_lossy(&output.stdout).parse()?)
}

/// The record, if there is one.
fn read_record(path: &Path) -> Result<Option<Record>> {
    match std::fs::read_to_string(path) {
        Ok(json) => Ok(Some(
            Record::parse(&json).with_context(|| path.display().to_string())?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| path.display().to_string()),
    }
}

/// Removes the record once its machine is done; another machine's stays.
fn remove_record(path: &Path, name: &MachineName) -> Result<()> {
    match read_record(path)? {
        Some(record) if &record.machine == name => {
            std::fs::remove_file(path).with_context(|| path.display().to_string())
        }
        Some(_) | None => Ok(()),
    }
}

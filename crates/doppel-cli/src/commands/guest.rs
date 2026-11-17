//! `screenshot`, `input`, `exec` and `cp`: one running machine's guest,
//! directly from this Mac.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use doppel_machine::files::FileMode;
use doppel_machine::geometry::Point;
use doppel_machine::guest_path::GuestPath;
use doppel_machine::input::{Button, ClickCount, Input, KeyChord};
use doppel_machine::machine::MachineName;
use doppel_protocol::transport::Cancel;

use crate::context::Context;
use crate::copy::{Copy, CopyEnd};
use crate::report::Report;

/// Save a host-side screenshot (works before login, no guest agent needed).
#[derive(Debug, Args)]
pub(crate) struct ScreenshotCmd {
    name: MachineName,
    #[arg(long, short)]
    output: PathBuf,
}

impl ScreenshotCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        let by = machines.deadline();
        let image = machines
            .machine(&self.name)
            .running(by)?
            .screenshot(0, by)?;
        std::fs::write(&self.output, &image.data)
            .with_context(|| self.output.display().to_string())?;
        Ok(Report::Screenshot {
            note: image.coordinate_note(),
            path: self.output,
        })
    }
}

/// Send input through the VM's own keyboard and pointer (works before login).
#[derive(Debug, Args)]
pub(crate) struct InputCmd {
    name: MachineName,
    #[command(subcommand)]
    action: InputAction,
}

impl InputCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        let by = machines.deadline();
        machines
            .machine(&self.name)
            .running(by)?
            .host_input(&self.action.parse()?, by)?;
        Ok(Report::InputSent(self.name))
    }
}

#[derive(Debug, Subcommand)]
enum InputAction {
    /// Click at guest display points.
    Click {
        x: f64,
        y: f64,
        #[arg(long)]
        right: bool,
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Type text (US keyboard layout through the host path).
    Type { text: String },
    /// Press a key chord, e.g. return, cmd+space, cmd+shift+4.
    Key { chord: KeyChord },
}

impl InputAction {
    fn parse(self) -> Result<Input> {
        Ok(match self {
            Self::Click { x, y, right, count } => Input::Click {
                at: Point::new(x, y)?,
                button: if right { Button::Right } else { Button::Left },
                count: ClickCount::try_from(count)?,
            },
            Self::Type { text } => Input::Type(text),
            Self::Key { chord } => Input::Key(chord),
        })
    }
}

/// Run a program in the guest through its agent; exits with the program's status.
#[derive(Debug, Args)]
pub(crate) struct ExecCmd {
    name: MachineName,
    /// Stop the program after this many milliseconds.
    #[arg(long, default_value_t = 10_000)]
    timeout_ms: u64,
    /// Absolute guest working directory.
    #[arg(long)]
    cwd: Option<GuestPath>,
    /// Absolute guest path of the program, then its arguments.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

impl ExecCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        use doppel_machine::exec::{Completeness, ExecRequest, Termination};
        let (program, arguments) = self
            .command
            .split_first()
            .context("give the program to run")?;
        let mut request = ExecRequest::new(
            program.parse()?,
            arguments.to_vec(),
            Duration::from_millis(self.timeout_ms),
            ExecRequest::MAX_OUTPUT,
        )?;
        if let Some(cwd) = self.cwd {
            request = request.in_directory(cwd);
        }
        let machines = ctx.machines();
        let by = machines.deadline();
        let output = machines
            .machine(&self.name)
            .running(by)?
            .agent(by)?
            .exec(&request, &Cancel::never())?;
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stderr().write_all(&output.stderr)?;
        if output.completeness != Completeness::Full {
            eprintln!("doppel exec: output truncated");
        }
        match output.termination {
            Termination::Exited(code) => Ok(Report::Exit(code)),
            Termination::Signaled(signal) => {
                eprintln!("doppel exec: killed by signal {signal}");
                Ok(Report::Exit(128 + signal))
            }
            Termination::TimedOut => {
                bail!(
                    "timed out after {} ms; the program was stopped",
                    self.timeout_ms
                )
            }
        }
    }
}

/// Copy a file between the host and a guest: `doppel cp notes.txt dev:/tmp/notes.txt`
/// or `doppel cp dev:/tmp/out.log ./out.log`. At most 32 MiB.
#[derive(Debug, Args)]
pub(crate) struct CpCmd {
    source: CopyEnd,
    destination: CopyEnd,
    /// Replace the destination if it exists (by default an existing file is left alone).
    #[arg(long)]
    replace: bool,
    /// Octal permissions for a file written in the guest.
    #[arg(long, default_value = "644", value_parser = octal_mode)]
    mode: FileMode,
}

impl CpCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        use doppel_machine::files::{MAX_FILE_BYTES, ReadRequest, WritePolicy, WriteRequest};
        let machines = ctx.machines();
        let policy = if self.replace {
            WritePolicy::Replace
        } else {
            WritePolicy::CreateNew
        };
        match Copy::between(self.source, self.destination)? {
            Copy::Upload { from, machine, to } => {
                let data = std::fs::read(&from).with_context(|| from.display().to_string())?;
                let bytes = data.len();
                let request = WriteRequest::new(to, data, self.mode, policy)?;
                let by = machines.deadline();
                machines
                    .machine(&machine)
                    .running(by)?
                    .agent(by)?
                    .write_file(&request, by)?;
                Ok(Report::Uploaded {
                    machine,
                    path: request.path,
                    bytes,
                })
            }
            Copy::Download { machine, from, to } => {
                let by = machines.deadline();
                let data = machines
                    .machine(&machine)
                    .running(by)?
                    .agent(by)?
                    .read_file(&ReadRequest::new(from, MAX_FILE_BYTES)?, by)?;
                let mut options = std::fs::OpenOptions::new();
                match policy {
                    WritePolicy::CreateNew => options.write(true).create_new(true),
                    WritePolicy::Replace => options.write(true).create(true).truncate(true),
                };
                let mut file = options.open(&to).map_err(|error| {
                    let hint = if error.kind() == std::io::ErrorKind::AlreadyExists {
                        " (use --replace to overwrite)"
                    } else {
                        ""
                    };
                    anyhow::anyhow!("{}: {error}{hint}", to.display())
                })?;
                file.write_all(&data)
                    .with_context(|| to.display().to_string())?;
                Ok(Report::Downloaded {
                    path: to,
                    bytes: data.len(),
                })
            }
        }
    }
}

/// A guest file's permissions, as `--mode` takes them: octal, `0o` optional.
fn octal_mode(raw: &str) -> Result<FileMode> {
    let bits = u32::from_str_radix(raw.strip_prefix("0o").unwrap_or(raw), 8)
        .with_context(|| format!("{raw:?} is not octal"))?;
    Ok(FileMode::new(bits)?)
}

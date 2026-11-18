//! `doppel`: the outermost shell. Parses arguments and environment once,
//! wires capabilities, and prints results. `main.rs` only sets up the
//! process and runs [`Cli`].
//!
//! Each command lives in [`commands`]: its arguments and the `run` that
//! does it with the [`Context`] built from the global flags.

use std::path::PathBuf;
use std::process::ExitCode;

mod commands;
mod context;
mod copy;
mod daemon_at;
mod doctor;
mod error_message;
mod export;
mod fork_run;
mod launchd;
mod mcp;
mod reel;
mod report;
mod signals;
mod tokens;
mod view;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::commands::{
    agent, checkpoints, daemon, debug, golden, guest, lifecycle, machines, record, sessions,
};
use crate::context::Context;
pub use crate::error_message::ErrorMessage;
use crate::report::Report;

/// The `doppel` command line: what to do, and where machines are.
#[derive(Debug, Parser)]
#[command(
    name = "doppel",
    version,
    about = "A double of your Mac for AI agents."
)]
pub struct Cli {
    /// Where machines are stored.
    #[arg(long, env = "DOPPEL_HOME", global = true)]
    home: Option<PathBuf>,
    /// Directory for VMM sockets (kept short: unix socket paths max out at 103 bytes).
    #[arg(long, env = "DOPPEL_RUNTIME_DIR", global = true)]
    runtime_dir: Option<PathBuf>,
    /// The signed doppel-vmm binary. Defaults to the one next to `doppel`.
    #[arg(long, env = "DOPPEL_VMM", global = true)]
    vmm: Option<PathBuf>,
    /// Use the daemon listening on this socket instead of this store's:
    /// another Mac's, forwarded with `ssh -L`. Commands that go through a
    /// daemon then act on that Mac's machines; `view` reads their displays
    /// encoded.
    #[arg(long, global = true, value_name = "SOCKET")]
    host: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Ls(machines::LsCmd),
    Create(machines::CreateCmd),
    Import(machines::ImportCmd),
    Start(lifecycle::StartCmd),
    Login(lifecycle::LoginCmd),
    SetPassword(lifecycle::SetPasswordCmd),
    ForgetPassword(lifecycle::ForgetPasswordCmd),
    Status(machines::StatusCmd),
    /// Record a machine's display to a video file.
    #[command(subcommand)]
    Record(record::RecordCommand),
    View(commands::view::ViewCmd),
    Stop(lifecycle::StopCmd),
    Pause(lifecycle::PauseCmd),
    Resume(lifecycle::ResumeCmd),
    Checkpoint(checkpoints::CheckpointCmd),
    Checkpoints(checkpoints::CheckpointsCmd),
    ForgetCheckpoint(checkpoints::ForgetCheckpointCmd),
    Fork(checkpoints::ForkCmd),
    ForkRun(checkpoints::ForkRunCmd),
    Restore(checkpoints::RestoreCmd),
    Screenshot(guest::ScreenshotCmd),
    Input(guest::InputCmd),
    Exec(guest::ExecCmd),
    Cp(guest::CpCmd),
    /// doppeld, the daemon that owns your machines (docs/doppeld.md): `ls`,
    /// `status`, `start`, `login`, `stop`, `pause`, `resume`, `checkpoint`,
    /// `forget-checkpoint`, `fork`, `fork-run`, `restore`, `sessions` and
    /// `mcp` go through it. `install` has
    /// launchd start it when they need it.
    #[command(subcommand)]
    Daemon(daemon::DaemonCommand),
    Sessions(sessions::SessionsCmd),
    Mcp(commands::mcp::McpCmd),
    /// The guest agent.
    #[command(subcommand)]
    Agent(agent::AgentCommand),
    /// Golden images: machines to fork from.
    #[command(subcommand)]
    Golden(golden::GoldenCommand),
    Doctor(commands::doctor::DoctorCmd),
    /// Diagnostics.
    #[command(subcommand)]
    Debug(debug::DebugCommand),
}

/// Where a command does its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Through a daemon, which may be another Mac's.
    Daemon,
    /// On this Mac's store, or this Mac's daemon itself.
    ThisMac,
}

impl Command {
    const fn reach(&self) -> Reach {
        match self {
            Self::Ls(_)
            | Self::Start(_)
            | Self::Login(_)
            | Self::Status(_)
            | Self::View(_)
            | Self::Stop(_)
            | Self::Pause(_)
            | Self::Resume(_)
            | Self::Checkpoint(_)
            | Self::Fork(_)
            | Self::ForkRun(_)
            | Self::Restore(_)
            | Self::ForgetCheckpoint(_)
            | Self::Sessions(_)
            | Self::Mcp(_) => Reach::Daemon,
            Self::Create(_)
            | Self::Import(_)
            | Self::SetPassword(_)
            | Self::ForgetPassword(_)
            | Self::Record(_)
            | Self::Checkpoints(_)
            | Self::Screenshot(_)
            | Self::Input(_)
            | Self::Exec(_)
            | Self::Cp(_)
            | Self::Daemon(_)
            | Self::Agent(_)
            | Self::Golden(_)
            | Self::Doctor(_)
            | Self::Debug(_) => Reach::ThisMac,
        }
    }

    fn run(self, ctx: Context) -> Result<Report> {
        match self {
            Self::Ls(cmd) => cmd.run(&ctx),
            Self::Create(cmd) => cmd.run(&ctx),
            Self::Import(cmd) => cmd.run(&ctx),
            Self::Start(cmd) => cmd.run(&ctx),
            Self::Login(cmd) => cmd.run(&ctx),
            Self::SetPassword(cmd) => cmd.run(&ctx),
            Self::ForgetPassword(cmd) => cmd.run(&ctx),
            Self::Status(cmd) => cmd.run(&ctx),
            Self::Record(cmd) => cmd.run(&ctx),
            Self::View(cmd) => cmd.run(&ctx),
            Self::Stop(cmd) => cmd.run(&ctx),
            Self::Pause(cmd) => cmd.run(&ctx),
            Self::Resume(cmd) => cmd.run(&ctx),
            Self::Checkpoint(cmd) => cmd.run(&ctx),
            Self::Checkpoints(cmd) => cmd.run(&ctx),
            Self::ForgetCheckpoint(cmd) => cmd.run(&ctx),
            Self::Fork(cmd) => cmd.run(&ctx),
            Self::ForkRun(cmd) => cmd.run(&ctx),
            Self::Restore(cmd) => cmd.run(&ctx),
            Self::Screenshot(cmd) => cmd.run(&ctx),
            Self::Input(cmd) => cmd.run(&ctx),
            Self::Exec(cmd) => cmd.run(&ctx),
            Self::Cp(cmd) => cmd.run(&ctx),
            Self::Daemon(cmd) => cmd.run(ctx),
            Self::Sessions(cmd) => cmd.run(&ctx),
            Self::Mcp(cmd) => cmd.run(&ctx),
            Self::Agent(cmd) => cmd.run(&ctx),
            Self::Golden(cmd) => cmd.run(&ctx),
            Self::Doctor(cmd) => cmd.run(&ctx),
            Self::Debug(cmd) => cmd.run(&ctx),
        }
    }
}

impl Cli {
    /// Runs the command it was given, prints what it did (or why it
    /// failed, on stderr), and gives the status to exit with.
    #[must_use]
    pub fn run(self) -> ExitCode {
        match self.report() {
            Ok(report) => {
                print!("{report}");
                match report.exit() {
                    // The status a program run in the guest exited with,
                    // as the OS keeps it: its low byte.
                    Some(code) => ExitCode::from(code.to_le_bytes()[0]),
                    None => ExitCode::SUCCESS,
                }
            }
            // Each cause once: Doppel's errors already say theirs.
            Err(error) => {
                eprintln!("Error: {}", ErrorMessage::of(&error));
                ExitCode::FAILURE
            }
        }
    }

    fn report(self) -> Result<Report> {
        if self.host.is_some() && self.command.reach() == Reach::ThisMac {
            bail!(
                "--host is for commands that go through a daemon; this one works on this Mac only"
            );
        }
        let ctx = Context::new(self.home, self.runtime_dir, self.vmm, self.host)?;
        self.command.run(ctx)
    }
}

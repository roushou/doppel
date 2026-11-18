//! `start`, `login`, `stop`, `pause` and `resume`, through the daemon; and
//! the stored login password the daemon types (`set-password`,
//! `forget-password`).

use anyhow::{Result, bail};
use clap::Args;
use doppel_client::{Client, StopMode, Window};
use doppel_host::capabilities::SystemClock;
use doppel_host::machines::CLAIM_TIMEOUT;
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::daemon_at::{LIFECYCLE, within};
use crate::report::Report;

/// Start a machine (resuming saved state if it has one) and wait for an
/// agent-ready desktop: if the guest is locked, the stored password
/// (`doppel set-password`) is typed once.
#[derive(Debug, Args)]
pub(crate) struct StartCmd {
    name: MachineName,
    /// Show the VM in a native window.
    #[arg(long)]
    window: bool,
    /// Return once the VM runs, without waiting for the desktop.
    #[arg(long)]
    no_wait: bool,
}

impl StartCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let client = ctx.daemon()?.connect()?;
        let window = if self.window {
            Window::Shown
        } else {
            Window::Hidden
        };
        let (state, next) = client.start(&self.name, window, within(LIFECYCLE))?;
        let started = Report::Started {
            name: self.name.clone(),
            state,
            next,
        };
        if self.no_wait {
            Ok(started)
        } else {
            // Said before the wait, which may be long.
            print!("{started}");
            await_desktop(&client, self.name)
        }
    }
}

/// Wait for an agent-ready desktop on a running machine, typing the
/// stored password once if the guest is locked.
#[derive(Debug, Args)]
pub(crate) struct LoginCmd {
    name: MachineName,
}

impl LoginCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        await_desktop(&ctx.daemon()?.connect()?, self.name)
    }
}

/// Waits for an agent-ready desktop; fails unless it got one.
pub(crate) fn await_desktop(client: &Client<SystemClock>, name: MachineName) -> Result<Report> {
    eprintln!("{name}: waiting for the desktop…");
    let desktop = client.await_desktop(&name, within(LIFECYCLE))?;
    if desktop.is_ready() {
        Ok(Report::Ready { name, desktop })
    } else {
        bail!("{name}: not ready: {desktop}")
    }
}

#[derive(Debug, Args)]
pub(crate) struct StopCmd {
    name: MachineName,
    /// Power off immediately instead of asking the guest to shut down.
    #[arg(long)]
    force: bool,
}

impl StopCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let mode = if self.force {
            StopMode::Force
        } else {
            StopMode::Graceful
        };
        let report = ctx
            .daemon()?
            .connect()?
            .stop(&self.name, mode, within(LIFECYCLE))?;
        Ok(Report::Stopped {
            name: self.name,
            report,
        })
    }
}

#[derive(Debug, Args)]
pub(crate) struct PauseCmd {
    name: MachineName,
}

impl PauseCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        ctx.daemon()?
            .connect()?
            .pause(&self.name, within(LIFECYCLE))?;
        Ok(Report::Paused(self.name))
    }
}

#[derive(Debug, Args)]
pub(crate) struct ResumeCmd {
    name: MachineName,
}

impl ResumeCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let resumed = ctx
            .daemon()?
            .connect()?
            .resume(&self.name, within(LIFECYCLE))?;
        Ok(Report::Resumed {
            name: self.name,
            resumed,
        })
    }
}

/// Store the guest user's login password (read from stdin, not echoed)
/// in the machine's bundle, readable by you only.
#[derive(Debug, Args)]
pub(crate) struct SetPasswordCmd {
    name: MachineName,
}

impl SetPasswordCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        use doppel_host::login::{PasswordFile, SecretPrompt};
        use doppel_machine::login::LoginPassword;
        let machines = ctx.machines();
        let name = self.name;
        let _claim = machines.claim(&name, machines.within(CLAIM_TIMEOUT))?;
        let bundle = machines.store().bundle(&name);
        if !bundle.dir().is_dir() {
            bail!("no machine '{name}'");
        }
        let line = SecretPrompt.read_line(&format!("{name}: guest login password: "))?;
        let password = LoginPassword::parse(&line)?;
        let path = PasswordFile.write(&bundle, &password)?;
        Ok(Report::PasswordSaved { name, path })
    }
}

/// Delete the stored login password.
#[derive(Debug, Args)]
pub(crate) struct ForgetPasswordCmd {
    name: MachineName,
}

impl ForgetPasswordCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let machines = ctx.machines();
        let name = self.name;
        let _claim = machines.claim(&name, machines.within(CLAIM_TIMEOUT))?;
        Ok(
            match doppel_host::login::PasswordFile.forget(&machines.store().bundle(&name))? {
                true => Report::PasswordDeleted(name),
                false => Report::NoPassword(name),
            },
        )
    }
}

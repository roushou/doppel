//! `daemon`: doppeld itself, its LaunchAgent, its browser listener and the
//! tokens that let browsers in.

use anyhow::{Result, bail};
use clap::Subcommand;
use doppel_host::capabilities::{OsEntropy, SystemClock, WallClock as _};

use crate::context::Context;
use crate::report::Report;
use crate::{launchd, signals, tokens};

/// doppeld, the daemon that owns your machines (docs/doppeld.md): `ls`,
/// `status`, `start`, `login`, `stop`, `pause`, `resume`, `checkpoint`,
/// `forget-checkpoint`, `fork`, `fork-run`, `restore`, `sessions` and
/// `mcp` go through it. `install` has
/// launchd start it when they need it.
#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    /// Run the daemon in the foreground until it's stopped (Ctrl-C).
    Run {
        /// Serve the socket launchd bound, as the installed LaunchAgent does.
        #[arg(long)]
        launchd: bool,
    },
    /// Install doppeld as a LaunchAgent: launchd listens on its socket from
    /// login and starts it on the first connection.
    Install,
    /// Unload and remove the LaunchAgent. Machines it started keep running.
    Uninstall,
    /// Whether the LaunchAgent is installed and the daemon runs.
    Status,
    /// Stop the running daemon; the next client starts it again.
    Stop,
    /// The end of the daemon's log.
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    /// Issue a token for the browser viewer. It's printed once, on stdout;
    /// the daemon keeps only its digest.
    Token {
        /// How long it lasts: `90m`, `12h`, `7d` (at most 30 days).
        #[arg(long, default_value = "12h")]
        ttl: doppel_gateway::Ttl,
        /// What to call it, to tell tokens apart ("phone").
        #[arg(long)]
        label: Option<doppel_gateway::TokenLabel>,
    },
    /// The tokens that let their holders in now.
    Tokens,
    /// Revoke a token: it lets no one in from now on.
    Revoke {
        /// Its id, as `doppel daemon tokens` lists it.
        id: doppel_gateway::TokenId,
    },
    /// The browser viewer's listener: HTTPS and its WebSocket, for holders
    /// of a token.
    #[command(subcommand)]
    Web(WebCommand),
}

impl DaemonCommand {
    /// Takes the context: `run` keeps its machines.
    pub(crate) fn run(self, ctx: Context) -> Result<Report> {
        Ok(match self {
            Self::Run { launchd } => {
                let source = if launchd {
                    SocketSource::Launchd
                } else {
                    SocketSource::Bind
                };
                serve(ctx, source)?
            }
            Self::Install => {
                let service = ctx.service()?;
                service.install()?;
                Report::Service(service.status()?)
            }
            Self::Uninstall => {
                ctx.service()?.uninstall()?;
                Report::DaemonUninstalled
            }
            Self::Status => Report::Service(ctx.service()?.status()?),
            Self::Stop => match ctx.service()?.stop()? {
                Some(pid) => Report::DaemonStopped(pid),
                None => Report::DaemonNotRunning,
            },
            Self::Logs { lines } => Report::Verbatim(ctx.service()?.log_tail(lines)?),
            Self::Token { ttl, label } => {
                tokens::issue(
                    &ctx.daemon()?,
                    ttl,
                    label.as_ref(),
                    &SystemClock,
                    ctx.machines().deadline(),
                )?;
                // Printed by `issue`: a secret never sits in a report.
                Report::Nothing
            }
            Self::Tokens => Report::Tokens {
                tokens: ctx.daemon()?.connect()?.tokens(ctx.machines().deadline())?,
                now: SystemClock.wall(),
            },
            Self::Revoke { id } => Report::Revoked(
                ctx.daemon()?
                    .connect()?
                    .revoke_token(&id, ctx.machines().deadline())?,
            ),
            Self::Web(command) => command.run(&ctx)?,
        })
    }
}

/// Where the daemon's listening socket comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketSource {
    /// The daemon binds it.
    Bind,
    /// launchd bound it and passes it in.
    Launchd,
}

/// Runs the daemon until a stop signal.
fn serve(ctx: Context, source: SocketSource) -> Result<Report> {
    let signals = signals::StopSignals::block()?;
    let daemon = doppel_daemon::Daemon::new(
        ctx.into_machines(),
        Box::new(doppel_daemon::EntropyMacs::new(OsEntropy)),
    );
    let listener = match source {
        SocketSource::Bind => daemon.listen()?,
        SocketSource::Launchd => daemon.adopt(launchd::Launchd::socket()?)?,
    };
    eprintln!("doppeld: listening on {}", listener.path().display());
    signals.stop(daemon.stopper())?;
    daemon.serve(&listener)?;
    eprintln!("doppeld: stopped");
    Ok(Report::Nothing)
}

#[derive(Debug, Subcommand)]
pub(crate) enum WebCommand {
    /// Listen on every address, now and whenever the daemon runs.
    On {
        #[arg(long, default_value_t = 7443)]
        port: u16,
    },
    /// Stop listening, and close every browser's connection.
    Off,
    /// Whether it listens, where, and its certificate's fingerprint.
    Status,
}

impl WebCommand {
    fn run(self, ctx: &Context) -> Result<Report> {
        let client = ctx.daemon()?.connect()?;
        let by = ctx.machines().deadline();
        let status = match self {
            Self::On { port } => client.configure_web(Some(port), by)?,
            Self::Off => client.configure_web(None, by)?,
            Self::Status => client.web_status(by)?,
        };
        match status {
            doppel_client::WebStatus::Off => Ok(Report::WebOff),
            doppel_client::WebStatus::Listening {
                port,
                fingerprint,
                names,
            } => Ok(Report::WebListening {
                port,
                fingerprint,
                names,
            }),
            doppel_client::WebStatus::Failed { port, reason } => {
                bail!("doppeld: web: not listening on port {port}: {reason}")
            }
        }
    }
}

//! `sessions`: who drives which machines through the daemon.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::context::Context;
use crate::report::Report;

/// The sessions open on the daemon: who drives which machines. With a
/// subcommand, pause or resume one.
#[derive(Debug, Args)]
pub(crate) struct SessionsCmd {
    #[command(subcommand)]
    action: Option<SessionsCommand>,
}

#[derive(Debug, Subcommand)]
enum SessionsCommand {
    /// Hold a session's next calls, whatever machine they name; one already
    /// running finishes.
    Pause { id: u64 },
    /// Let a paused session's calls go on.
    Resume { id: u64 },
}

impl SessionsCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let client = ctx.daemon()?.connect()?;
        let by = ctx.machines().deadline();
        Ok(match self.action {
            None => Report::Sessions(client.sessions(by)?),
            Some(SessionsCommand::Pause { id }) => {
                client.pause_session(id.into(), by)?;
                Report::SessionPaused(id)
            }
            Some(SessionsCommand::Resume { id }) => {
                client.resume_session(id.into(), by)?;
                Report::SessionResumed(id)
            }
        })
    }
}

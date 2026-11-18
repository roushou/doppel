//! `view`: the desktop window.

use anyhow::Result;
use clap::Args;
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::daemon_at::DaemonAt;
use crate::report::Report;
use crate::view;

/// Open the desktop window: machines, agents, screens and recordings. The named
/// machines open on stage; otherwise the first running one.
#[derive(Debug, Args)]
pub(crate) struct ViewCmd {
    names: Vec<MachineName>,
    /// Read each display as H.264 from its VMM rather than from the
    /// memory it shares, as a console on another Mac must.
    #[arg(long)]
    encoded: bool,
}

impl ViewCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let at = ctx.daemon()?;
        // Shared memory can't cross to another Mac.
        let frames = match (&at, self.encoded) {
            (DaemonAt::Forwarded(_), _) | (DaemonAt::Store(_), true) => view::Frames::Encoded,
            (DaemonAt::Store(_), false) => view::Frames::Shared,
        };
        // Replay reads videos from the store: on this Mac only.
        let videos = match &at {
            DaemonAt::Store(_) => view::Videos::Here,
            DaemonAt::Forwarded(_) => view::Videos::Elsewhere,
        };
        view::View::new(self.names, frames, videos, ctx.home()).run(at.connect()?, at.socket())?;
        Ok(Report::Nothing)
    }
}

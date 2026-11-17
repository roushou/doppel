//! `mcp`: serve MCP over stdio, relayed to the daemon.

use anyhow::Result;
use clap::Args;
use doppel_machine::machine::MachineName;

use crate::context::Context;
use crate::mcp::{DefaultMachine, Relay};
use crate::report::Report;

/// Serve MCP over stdio, for AI agents (e.g. `claude mcp add doppel -- doppel mcp --from ready`).
/// The calls go to the daemon as one session.
#[derive(Debug, Args)]
pub(crate) struct McpCmd {
    /// Machine that tools target when a call names none.
    #[arg(long, env = "DOPPEL_MACHINE", conflicts_with = "from")]
    machine: Option<MachineName>,
    /// Give the session a machine of its own: a fresh fork of this
    /// golden image's template, started in the background, which tools
    /// target when a call names none. It is powered off and deleted when
    /// the session ends, however this process ends.
    #[arg(long, value_name = "GOLDEN")]
    from: Option<MachineName>,
    /// Record the session into the store's `recordings/`: a video per
    /// VM run of each machine the calls touch, with captions of the
    /// calls, and a trace of them (`trace.jsonl`).
    /// From the environment, `DOPPEL_RECORD=1` (or `true`, `yes`, `on`).
    #[arg(long, env = "DOPPEL_RECORD", value_parser = clap::builder::BoolishValueParser::new())]
    record: bool,
}

impl McpCmd {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        let record = if self.record {
            doppel_client::Record::IntoStore
        } else {
            doppel_client::Record::Off
        };
        // clap refuses both together.
        let default = match (self.from, self.machine) {
            (Some(golden), _) => DefaultMachine::MadeFrom(golden),
            (None, Some(name)) => DefaultMachine::Named(name),
            (None, None) => DefaultMachine::None,
        };
        Relay::connect(&ctx.daemon()?, record, default)?.serve()?;
        Ok(Report::Nothing)
    }
}

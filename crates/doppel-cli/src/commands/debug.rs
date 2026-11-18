//! `debug`: diagnostics.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Subcommand, ValueEnum};
use doppel_host::capabilities::{Clock as _, SystemClock};
use doppel_machine::machine::MachineName;
use doppel_machine::vm::VsockPort;

use crate::context::Context;
use crate::report::Report;

#[derive(Debug, Subcommand)]
pub(crate) enum DebugCommand {
    /// Echo round trips through a guest vsock port over a handed-over socket.
    VsockEcho {
        name: MachineName,
        #[arg(long, default_value_t = 5000)]
        port: u32,
        #[arg(long, default_value_t = 100)]
        count: u32,
    },
    /// Take an AX snapshot through the guest agent: print the compacted listing,
    /// and optionally save the raw snapshot (protobuf) as a test fixture.
    AxDump {
        name: MachineName,
        #[arg(long, value_enum, default_value_t = DumpScope::Window)]
        scope: DumpScope,
        #[arg(long, value_enum, default_value_t = DumpMenus::Exclude)]
        menus: DumpMenus,
        #[arg(long, default_value_t = 4000)]
        max_tokens: u32,
        /// Write the agent's raw AxSnapshot here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DumpScope {
    /// The frontmost app's focused window.
    Window,
    /// Every window of the frontmost app.
    App,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum DumpMenus {
    Exclude,
    Include,
}

impl DebugCommand {
    pub(crate) fn run(self, ctx: &Context) -> Result<Report> {
        match self {
            Self::VsockEcho { name, port, count } => vsock_echo(ctx, &name, port, count),
            Self::AxDump {
                name,
                scope,
                menus,
                max_tokens,
                out,
            } => ax_dump(ctx, &name, scope, menus, max_tokens, out),
        }
    }
}

fn ax_dump(
    ctx: &Context,
    name: &MachineName,
    scope: DumpScope,
    menus: DumpMenus,
    max_tokens: u32,
    out: Option<PathBuf>,
) -> Result<Report> {
    use doppel_desktop::ax::compact::{CompactSnapshot, CompactionPolicy, Tokens};
    use doppel_desktop::ax::refs::Generation;
    use doppel_desktop::ax::tree::{AxSnapshot, Menus, Scope, SnapshotRequest};
    use prost::Message as _;

    let machines = ctx.machines();
    let by = machines.deadline();
    let mut agent = machines.machine(name).running(by)?.agent(by)?;
    let scope = match scope {
        DumpScope::Window => Scope::FocusedWindow,
        DumpScope::App => Scope::App(
            agent
                .status(by)?
                .frontmost
                .context("the guest has no frontmost app")?
                .pid,
        ),
    };
    let menus = match menus {
        DumpMenus::Exclude => Menus::Exclude,
        DumpMenus::Include => Menus::Include,
    };
    let clock = SystemClock;
    let started = clock.now();
    let wire = agent.ax_snapshot_wire(&SnapshotRequest::new(scope, menus), by)?;
    let walked = clock.now().duration_since(started);
    if let Some(out) = &out {
        std::fs::write(out, wire.encode_to_vec())
            .with_context(|| format!("writing {}", out.display()))?;
    }
    let snapshot = AxSnapshot::try_from(wire)?;
    let compact = CompactSnapshot::build(
        &snapshot,
        Generation::FIRST,
        CompactionPolicy {
            budget: Tokens::budget(max_tokens)?,
            ..CompactionPolicy::default()
        },
    );
    eprintln!(
        "{} AX nodes in {:.0} ms ({}); {} shown, {} omitted, ~{}{}",
        snapshot.tree.len(),
        walked.as_secs_f64() * 1000.0,
        snapshot.truncation,
        compact.shown,
        compact.omitted,
        compact.tokens,
        out.map(|o| format!("; raw snapshot saved to {}", o.display()))
            .unwrap_or_default()
    );
    Ok(Report::Verbatim(compact.text))
}

fn vsock_echo(ctx: &Context, name: &MachineName, port: u32, count: u32) -> Result<Report> {
    let machines = ctx.machines();
    let by = machines.deadline();
    let mut stream = machines
        .machine(name)
        .running(by)?
        .open_agent_channel(VsockPort::new(port), by)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let clock = SystemClock;
    let mut samples = Vec::new();
    let payload = [7u8; 64];
    for _ in 0..count {
        let started = clock.now();
        stream.write_all(&(payload.len() as u32).to_le_bytes())?;
        stream.write_all(&payload)?;
        let mut back = [0u8; 68];
        stream.read_exact(&mut back)?;
        if back[4..] != payload {
            bail!("echo mismatch");
        }
        samples.push(clock.now().duration_since(started));
    }
    samples.sort();
    let p = |q: f64| {
        samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)].as_secs_f64() * 1000.0
    };
    Ok(Report::Echoed {
        count: samples.len(),
        p50_ms: p(0.5),
        p99_ms: p(0.99),
    })
}

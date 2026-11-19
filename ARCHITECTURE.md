# Architecture

Doppel is organised by component: each crate has one job, and inside a crate
each top-level module is one component. [docs/crates.md](docs/crates.md) has
the layering table and the reasons behind each boundary.

## Crates

- `doppel-desktop`: what is seen and done on a Mac's desktop (pure)
- `doppel-machine`: a machine, its files, lineage and procedures (pure)
- `doppel-scene`: what a console shows, decided without drawing (pure)
- `doppel-gateway`: serves the web console to token holders
- `doppel-protocol`: the wire: generated types, framing, `convert/`, transport
- `doppel-mcp`: the MCP tool surface, over a `Runner` port
- `doppel-host`: runs machines on this Mac: store, `vmm/`, agent, provisioning
- `doppel-client`: talks to doppeld
- `doppel-console`: the console window, desktop and browser
- `doppel-daemon`: doppeld, which owns a user's machines and serves them
- `doppel-cli`: the `doppel` command, and the composition root

## Dependency direction

`A → B` means A may import B; never the reverse.

- cli → daemon → {host, client, console} → {protocol, mcp}
- {protocol, mcp} → {scene, gateway} → machine → desktop

Inside the larger crates:

- daemon: `lib` → `requests` → {`activity`, `recording`, `web`, `serving`}
  → `call_events` → `scheduler`
- console: `app` → {`screen`, `route`, `input`, `answers`} → `ui`;
  `macos`/`web` (the `platform`) and `screen` meet at the frame feed
- scene: `film`, `panel`, `attention`, `recordings` → `timeline`
- host: provisioning procedures → `guest_step` → `machines` → {`vmm`,
  `agent`, `store`} → {`capabilities`, `files`, `guest_user`}

## Named concepts

One home each; extend it rather than adding a parallel one.

- A guest step (a write, or a run that must exit 0) and its runner:
  `doppel_host::guest_step`; the guest's account name: `guest_user`.
- Whether an agent answers yet: `AgentReach` in `machines/agent_connection`;
  waiting for something to be ready: `Machines::poll` in `machines/poll`.
- Whether a checkpoint is there: `doppel_machine::bundle::CheckpointPresence`.
- Whether a failed request may have happened: `Failure` on each error,
  `ClientError`'s included.
- What a picked action leads to, a question or an order: `app::ask::Next`.

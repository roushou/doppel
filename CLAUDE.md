# Doppel

The way AI agents use macOS: isolated macOS VMs (or the real Mac) behind one
typed API, served over MCP by a per-user daemon, with desktop and web
consoles for the people watching.

- **Direction:** [docs/roadmap.md](docs/roadmap.md) says what works and what
  comes next, in order. Start there, and update it when an item lands.
- **How it works:** [docs/mcp.md](docs/mcp.md) for the agent's tools,
  [docs/doppeld.md](docs/doppeld.md) for the daemon,
  [docs/crates.md](docs/crates.md) for which crate owns what.
- **Checks:** every change passes `scripts/check.sh`. To check on a live
  machine, rebuild with `scripts/build.sh`, restart the daemon with
  `launchctl kickstart -k gui/$(id -u)/dev.doppel.doppeld`, and drive one.
- **Structure:** the codebase is organised by component, not by feature.
  See [ARCHITECTURE.md](ARCHITECTURE.md).
  - A new feature usually touches several components; it does not get its
    own file holding all of them.
  - Never add a top-level module or crate without saying so explicitly,
    with a one-line justification.
  - No `utils.rs`, `helpers.rs` or `common.rs`. Shared code belongs to the
    component that owns the concept.
  - Respect dependency direction: lower components never import from
    higher ones.
  - If a file passes ~800 lines or a module mixes components, flag it
    instead of appending to it.
- **Types:**
  - Model fixed sets of states as enums, not strings, ints, or bool
    clusters.
  - Group parameters that travel together into a struct.
  - No traits with a single implementor unless needed for tests.
  - Prefer exhaustive `match` over `_ =>` on project enums.
- **Docs describe Doppel as it is.** No dated logs, milestone results or
  "we used to" in docs or comments; git keeps the history.

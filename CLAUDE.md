# Doppel

The way AI agents use macOS: isolated macOS VMs (or the real Mac) behind one
typed API, served over MCP by a per-user daemon, with desktop and browser
viewers for the people watching.

- **Direction:** [docs/roadmap.md](docs/roadmap.md) says what works and what
  comes next, in order. Start there, and update it when an item lands.
- **How it works:** [docs/mcp.md](docs/mcp.md) for the agent's tools,
  [docs/doppeld.md](docs/doppeld.md) for the daemon,
  [docs/crates.md](docs/crates.md) for which crate owns what.
- **Checks:** every change passes `scripts/check.sh`. To check on a live
  machine, rebuild with `scripts/build.sh`, restart the daemon with
  `launchctl kickstart -k gui/$(id -u)/dev.doppel.doppeld`, and drive one.
- **Docs describe Doppel as it is.** No dated logs, milestone results or
  "we used to" in docs or comments; git keeps the history.

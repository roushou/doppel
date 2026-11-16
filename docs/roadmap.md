# Roadmap

Where Doppel is going, in order. Each item says when it's done; anything
without a "done when" isn't ready to start. Update this file as items land:
move what's finished into "What works", and keep it free of dated logs.

## The goal

The way AI agents use macOS: a double of your Mac (an isolated VM you can
fork and roll back), or your real one, behind one typed API. An agent sees
the screen through accessibility snapshots with refs, recognized text and
screenshots; acts by ref or point; waits on evidence; runs commands and moves
files. People watch and take over from a desktop or browser viewer.

## What works

- **Machines:** create from an IPSW with no person (macOS 27 hosts: the
  account is made on first boot, logged in, SSH on), import, start, stop,
  pause, resume, checkpoint, fork, restore; delete from the viewers; delete a
  checkpoint (`doppel forget-checkpoint`). Golden images with a
  template checkpoint; forks resume agent-ready in about 11 s.
- **The agent's tools over MCP:** `status`, `screenshot`, `snapshot` with
  refs, `act`, `click`, `type`, `key`, `find_text`, `wait_for`, `exec`,
  `read_file`, `write_file`, `mark`, `start`, `stop`, `checkpoint`,
  `rollback`, `fork`; `snapshot` and `wait_for` can list only what changed.
- **doppeld** ([doppeld.md](doppeld.md)): one daemon per user owns the
  machines; sessions, per-machine call queues, takeover and hand back,
  recordings with traces.
- **Viewers:** a desktop viewer (GPUI) and a browser viewer, both showing
  what agents do over the screen, an elements tree, read text, and replay.
  The browser viewer also lights up the elements an agent reads.
- **Exports:** `doppel record export` writes a recorded session's video
  with its overlay burned in.
- **Remote:** another Mac's daemon over `ssh -L` and `--host`; the browser
  viewer over TLS with tokens; several forks side by side with `fork-run`.

## Next

### 1. A fresh install reaches agent-ready with no person

Today the guest agent's Accessibility and Screen Recording permissions are
granted by hand once, on a golden image, and its forks inherit them. A new
install from an IPSW still needs a person to type the account password into
two macOS prompts.

- Run `doppel agent bootstrap` live on a newly created machine: it installs
  the agent over SSH, and has only been unit-tested.
- Find a way to grant both permissions without a person, or to make the one
  remaining step a single documented command. Investigate before building:
  what macOS 27 allows for a VM guest (configuration profiles, the
  provisioning options, the guest's TCC database while it isn't running)
  decides the approach.

Done when: `doppel create NAME --ipsw … --user U`, then `doppel agent
bootstrap`, then `doppel golden prepare` gives a golden image whose forks
pass `status` with every permission granted, with nothing typed by a person.

### 2. Close the known gaps

- **Text recognition when Vision's accurate model fails.** macOS's accurate
  text recognizer sometimes fails on this Mac with `e5rtError … 13` until a
  restart, and the fast one keeps working. Decide whether `find_text` and
  text waits fall back to the fast recognizer and say so in the result.
  Done when: the decision is made and, if it's a fallback, a test covers it.
- **Conversions that drop information.** Check `doppel-protocol`'s
  `convert.rs` for `From` impls that lose values; each becomes lossless, a
  `TryFrom`, or a named method. Done when: every remaining `From` there is
  lossless.
- **Free functions on placeholder types** (`struct Window;`,
  `struct Launchd;`, the `Verdicts` trait): each becomes a method of the type
  that owns the state it serves, or a module function.

### 3. Chrome through its DevTools protocol

Web pages are most of what agents use, and Chrome's accessibility tree shows
page content only when launched with `--force-renderer-accessibility`. A CDP
passthrough to the guest's Chrome would give an agent the DOM, navigation and
network events. Not designed yet: start with a short design in `docs/`
(transport from host to guest, which CDP domains the MCP tools expose, how it
shows in traces and the viewers).

### 4. Isolated networks

A machine has NAT or, with `create --offline`, no network device. Between
them: a network that reaches only what a run allows (no internet, or a list
of hosts), so an agent's actions can't leak data or touch production. Not
designed yet.

### 5. Host mode

Drive the user's real Mac with the same tools, with consent scoped by app and
by action, and every call visible and interruptible as it is for a VM. The
agent protocol already allows the agent to run on the host; nothing else
exists. Not designed yet; the consent model is the first question.

### 6. Fleets

Many Macs, each with its daemon (two macOS VMs per Mac is Apple's limit),
used as one pool: placing runs, and a viewer across hosts. Not designed yet.

## How work is done here

- Every change passes `scripts/check.sh` (web, rustfmt, dprint, buf lint,
  clippy, cargo test, swift test).
- What can be checked on a live machine is: rebuild with `scripts/build.sh`,
  restart the daemon (`launchctl kickstart -k gui/$(id -u)/dev.doppel.doppeld`),
  and drive a machine.
- [crates.md](crates.md) says which crate owns what and which may depend on
  which. Pure decisions live in pure crates and modules, and are tested
  without I/O.

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
  template checkpoint; forks resume agent-ready in about 11 s. A resume
  from saved memory returns once screenshots show the guest as it is, not
  as it was saved.
- **The agent's tools over MCP:** `status`, `screenshot`, `snapshot` with
  refs, `act`, `click`, `type`, `key`, `find_text`, `wait_for`, `exec`,
  `read_file`, `write_file`, `mark`, `start`, `stop`, `checkpoint`,
  `rollback`, `fork`; `snapshot` and `wait_for` can list only what changed.
  `doppel mcp --from GOLDEN` gives each session a temporary machine, a
  fresh fork of a golden image deleted when the session ends; `doppel
  doctor` (and `--json`) says whether that can work here, and what to fix.
- **doppeld** ([doppeld.md](doppeld.md)): one daemon per user owns the
  machines; sessions, per-machine call queues, takeover and hand back,
  recordings with traces.
- **Viewer:** one window (GPUI), on the desktop and in a browser
  (WebAssembly), showing what agents do over the screen, an elements tree,
  read text, and replay. It draws the film's shapes (what exports burn
  in), including the elements an agent reads.
- **Exports:** `doppel record export` writes a recorded session's video
  with its overlay burned in.
- **Remote:** another Mac's daemon over `ssh -L` and `--host`; the browser
  viewer over TLS with tokens; several forks side by side with `fork-run`.

## Next

### 1. A fresh install reaches agent-ready with no person

`doppel agent grant` gives the guest agent Accessibility and Screen
Recording with no person: on a shut-down machine it writes the guest's own
records of both into its Data volume (the system TCC database, held to the
agent's code requirement, and replayd's capture approvals, so the "bypass
the private window picker" alert never comes up). It refuses a TCC layout it
doesn't know. Checked on a fork of a hand-made golden image: grants removed
offline are gone at the next boot, and written by `grant` they're all there,
with no alert.

- Run the whole path live on a machine new from an IPSW: `doppel create
  NAME --ipsw … --user U`, `doppel agent bootstrap`, `doppel stop`, `doppel
  agent grant`, `doppel start`, `doppel golden prepare`. `bootstrap` has only
  been unit-tested.
- Files `grant` creates in the guest take the host user's uid. They match the
  guest's first account (501) only when the host user's uid is 501 too;
  otherwise replayd can't read the approvals it writes. Write them as the
  guest's uid, or say so and refuse.

Done when: that path gives a golden image whose forks pass `status` with
every permission granted and no alert on screen, with nothing typed by a
person.

### 2. Close the known gaps

- **Unrecognized values that can pass for known ones.** `RunState`,
  `Session`, `Permission`, `Truncation` and `Completeness` keep a value this
  build doesn't know as a public raw number, which `convert.rs` writes back
  as received. Nothing stops building one with a known number (an
  `Unrecognized(2)` that encodes as `Running`), so those five `From`s are
  lossless only for values the parsers make. Done when: such a value can't
  be built, or the encoding says what it does with one.

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

- Every change passes `scripts/check.sh` (rustfmt, dprint, buf lint,
  clippy for the desktop and the browser, cargo test, swift test).
- What can be checked on a live machine is: rebuild with `scripts/build.sh`,
  restart the daemon (`launchctl kickstart -k gui/$(id -u)/dev.doppel.doppeld`),
  and drive a machine.
- [crates.md](crates.md) says which crate owns what and which may depend on
  which. Pure decisions live in pure crates and modules, and are tested
  without I/O.

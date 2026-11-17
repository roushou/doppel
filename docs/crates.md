# Crates

How the workspace is cut into crates, and which may depend on which. A
crate boundary is worth it only where it enforces a dependency set, serves a
distinct consumer, or changes on its own cadence.

## Layering

A crate depends only on crates in rows above it. Pure crates list only pure
dependencies, so the compiler holds their purity; a pure module inside a crate
that does I/O keeps a source test forbidding I/O and runtime imports.

| Layer | Crate             | Job                                                                                                                     | Pure |
| ----- | ----------------- | ----------------------------------------------------------------------------------------------------------------------- | ---- |
| 0     | `doppel-machine`  | Model a machine and its guest: spec, files, lineage, the procedures that change it, and what is seen and done inside it | yes  |
| 1     | `doppel-scene`    | What a viewer shows, decided without drawing: timelines, elements, panels, stage, recordings, streams, films            | yes  |
| 1     | `doppel-gateway`  | Serve the browser viewer to token holders                                                                               | no   |
| 2     | `doppel-protocol` | Doppel's wire: generated types, framing, conversions; `transport` carries it (unix only)                                | no   |
| 2     | `doppel-mcp`      | The MCP tool surface, over a `Runner` port                                                                              | no   |
| 3     | `doppel-host`     | Run machines on this Mac: store, VMM and agent clients, provisioning                                                    | no   |
| 3     | `doppel-client`   | Talk to doppeld                                                                                                         | no   |
| 3     | `doppel-viewer`   | The desktop viewer window (GPUI), over a `Daemon` port                                                                  | no   |
| 4     | `doppel-daemon`   | doppeld: owns a user's machines and serves them                                                                         | no   |
| 5     | `doppel-cli`      | The `doppel` command, and the composition root                                                                          | no   |

`doppel-protocol` builds for WebAssembly but for its `transport`. It sits
above `doppel-scene` because it converts the film's shapes to the `vmm.v1`
pictures an export burns in and the `film.v1` frames a viewer draws.

## Geometry

`doppel_machine::geometry` has one `Point` (finite, on the display), one
`Size` (finite, non-negative) and one `Rect` (a finite origin that may
overhang an edge, and a `Size`), all in display points. A mark's `Area` is a
`Rect` that must start on the display and have area. The viewer's `Position`
is a fraction of the screen, not display points, so it is its own type.

A display size reaches a viewer as `Option<Size>`: a stream or recording that
doesn't say, or says a size without area, has none, and its screen is
view-only, since a person's input can't be placed on it.

## Shared vocabulary

What the client, daemon, MCP server and scene all speak has one owner in
`doppel-machine`: input's `Via`, a screenshot's `ScreenshotSize`, a
snapshot's `SnapshotSpec`, and a machine's `MachineReport` with its
`AgentReport`. `doppel-protocol` converts the wire forms (`convert.rs`).

Activity has two vocabularies. `doppel_machine::activity` and
`::recording` are the stream and a recording's trace as parsed (by
`doppel-protocol`'s `convert`): they keep what this build doesn't know
(`Unknown(i32)`) and time in `Duration`. The scene's are what the overlay
draws: unknowns folded into `Other` or `Done`, failures into one `Failed`,
times in milliseconds on the window's clock. `doppel_scene::from_daemon`
maps one to the other, with tests, for the desktop viewer (through the
CLI's `view/port.rs`) and the browser's film alike.

## Storage

A bundle's `spec.pb` and `lineage.pb` use their own package,
`doppel.bundle.v1` (`proto/doppel/bundle/v1`), so a protocol change is never
a storage change. The files only ever gain fields; `fixtures/bundle/` pins
their bytes. `doppel-protocol::bundle` converts them.

## Viewers

Both viewers keep their decisions out of their toolkit, and test them
without it:

- **Per machine,** a panel holds the action under way (one at a time), the
  report while the machine is on the stage, and the timeline. In
  `doppel_scene::panel` it also holds the screen as a state machine
  (`Closed`, `Opening` with the last failure, `Open`, `Failed`), the
  overlay, the person's latest looks (only the latest answer lands) and
  control time; it is generic over the toolkit's screen and handles. The
  browser's `web/src/lib/panels.ts` holds control (`held` or `free`); its
  lens and looks stay in `Panel.svelte`, with the screen they're drawn over.
- **The stage** (`doppel_scene::stage`, `web/src/lib/stage.ts`) lays out the
  two bays: the slot picked, side by side, and the open launcher.

`crates/doppel-cli/src/view/fixtures.rs` writes `fixtures/scene/`: activity
events and recorded snapshots in, and what the Rust scene makes of them out,
in the page's shapes. `web/src/lib/scene.test.ts` replays the same bytes
through the page's code and must get the same answer: the timeline (moments,
lanes, scenes at instants) and the elements tree (each element, rows with
wrappers hidden and shown, the element under a grid of points). Regenerate
with `DOPPEL_UPDATE_GOLDEN=1 cargo test -p doppel-cli fixtures`.

## The command line

`doppel_cli::Cli` is the command line as a library: clap parses each
argument into its type, so a bad value is refused before anything runs, and
`Cli::run` returns a `Report` whose `Display` is everything `doppel` prints
on stdout. `main.rs` answers `SSH_ASKPASS` or runs the command and prints its
report. Progress and warnings go to stderr as they happen. `daemon token`
prints its token itself, so a secret never sits in a report.

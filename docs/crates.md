# Crates

How the workspace is cut into crates, and which may depend on which. A
crate boundary is worth it only where it enforces a dependency set, serves a
distinct consumer, or changes on its own cadence.

## Layering

A crate depends only on crates in rows above it. Pure crates list only pure
dependencies, so the compiler holds their purity; a pure module inside a crate
that does I/O keeps a source test forbidding I/O and runtime imports.

| Layer | Crate             | Job                                                                                                                | Pure |
| ----- | ----------------- | ------------------------------------------------------------------------------------------------------------------ | ---- |
| 0     | `doppel-desktop`  | Model what is seen and done on a Mac's desktop: screen and text, accessibility tree, input, files, programs, waits | yes  |
| 1     | `doppel-machine`  | Model a machine: spec, files, lineage, and the procedures that change it                                           | yes  |
| 2     | `doppel-scene`    | What a viewer shows, decided without drawing: timelines, elements, panels, stage, recordings, streams, films       | yes  |
| 2     | `doppel-gateway`  | Serve the browser viewer to token holders                                                                          | no   |
| 3     | `doppel-protocol` | Doppel's wire: generated types, framing, conversions; `transport` carries it (unix only)                           | no   |
| 3     | `doppel-mcp`      | The MCP tool surface, over a `Runner` port                                                                         | no   |
| 4     | `doppel-host`     | Run machines on this Mac: store, VMM and agent clients, provisioning                                               | no   |
| 4     | `doppel-client`   | Talk to doppeld                                                                                                    | no   |
| 4     | `doppel-viewer`   | The viewer window (GPUI), on the desktop and in a browser, over a `Daemon` port                                    | no   |
| 5     | `doppel-daemon`   | doppeld: owns a user's machines and serves them                                                                    | no   |
| 6     | `doppel-cli`      | The `doppel` command, and the composition root                                                                     | no   |

`doppel-protocol` builds for WebAssembly but for its `transport`, so the
viewer in a browser reads the same wire. It sits above `doppel-scene`
because it converts the film's shapes to the `vmm.v1` pictures an export
burns in, and the daemon's encoded frames to the scene's.

`doppel-viewer` is one window for both targets. What differs is one module
per target, under one name (`platform`): `macos` reads the daemon's shared
or encoded display on threads, decodes with VideoToolbox and plays videos
with AVFoundation; `web` reaches the daemon over the gateway's WebSocket,
decodes with WebCodecs and plays videos with a `<video>`, each through
`web/glue.js`. `scripts/build-web.sh` builds it for the browser into
`target/viewer-web`, with the page in `crates/doppel-viewer/page/`, and
`doppel-gateway` embeds that build. The build is for size (the `web`
profile), with brotli and gzip copies of each file; the gateway sends the
smallest copy a browser takes, with an ETag, so a browser that has the
file gets `304 Not Modified` instead. The desktop's `Daemon` port is the
CLI's (`view/port.rs`, over `doppel-client`); the browser's is the
viewer's own (`web/daemon.rs`), since no other crate runs there.

## Geometry

`doppel_desktop::geometry` has one `Point` (finite, on the display), one
`Size` (finite, non-negative) and one `Rect` (a finite origin that may
overhang an edge, and a `Size`), all in display points. A mark's `Area` is a
`Rect` that must start on the display and have area. The viewer's `Position`
is a fraction of the screen, not display points, so it is its own type.

A display size reaches a viewer as `Option<Size>`: a stream or recording that
doesn't say, or says a size without area, has none, and its screen is
view-only, since a person's input can't be placed on it.

## Shared vocabulary

What the client, daemon, MCP server and scene all speak has one owner:
`doppel-desktop` for input's `Via`, a screenshot's `ScreenshotSize`, a
snapshot's `SnapshotSpec` and the agent's `AgentReport`, and
`doppel-machine` for a machine's `MachineReport`. `doppel-protocol` converts the wire forms (`convert.rs`).

Activity has two vocabularies. `doppel_machine::activity` and
`::recording` are the stream and a recording's trace as parsed (by
`doppel-protocol`'s `convert`): they keep what this build doesn't know
(`Unknown(i32)`) and time in `Duration`. The scene's are what the overlay
draws: unknowns folded into `Other` or `Done`, failures into one `Failed`,
times in milliseconds on the window's clock. `doppel_scene::from_daemon`
maps one to the other, with tests, for the viewer on both targets.

## Storage

A bundle's `spec.pb` and `lineage.pb` use their own package,
`doppel.bundle.v1` (`proto/doppel/bundle/v1`), so a protocol change is never
a storage change. The files only ever gain fields; `fixtures/bundle/` pins
their bytes. `doppel-protocol::bundle` converts them.

## Viewers

The viewer keeps its decisions out of its toolkit, and tests them without
it:

- **Per machine,** a panel holds the action under way (one at a time), the
  report while the machine is on the stage, and the timeline. In
  `doppel_scene::panel` it also holds the screen as a state machine
  (`Closed`, `Opening` with the last failure, `Open`, `Failed`), the
  overlay, the person's latest looks (only the latest answer lands) and
  control time; it is generic over the toolkit's screen and handles.
- **The stage** (`doppel_scene::stage`) lays out the two bays: the slot
  picked, side by side, and the open launcher.
- **The daemon's answers** (`doppel_viewer::answers`) become the window's
  words the same way on both targets.
- **The window's place** (`doppel_viewer::route`) is `/`,
  `/machines/<name>`, `/replay` or `/replay/<id>`, parsed once from the
  address. The window holds it, and its page with it: one `navigate`
  changes both, for a click, a link, or Back and Forward. Each page is a
  module of `app/` (`machines`, `recordings`, `replay`). A machine that
  can't be shown is refused with why (`doppel_scene::stage::Unstaged`),
  never ignored. In a browser the address bar shows the place, and the
  gateway serves the page at each one; a desktop window has none.

## The command line

`doppel_cli::Cli` is the command line as a library: clap parses each
argument into its type, so a bad value is refused before anything runs, and
`Cli::run` returns a `Report` whose `Display` is everything `doppel` prints
on stdout. `main.rs` answers `SSH_ASKPASS` or runs the command and prints its
report. Progress and warnings go to stderr as they happen. `daemon token`
prints its token itself, so a secret never sits in a report.

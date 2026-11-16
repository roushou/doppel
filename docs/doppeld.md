# doppeld: the control plane

One long-lived process per user owns the machines, and every client (the CLI,
MCP clients, the desktop viewer, the browser viewer) goes through it, locally
or from another computer.

## What it builds on

- **One VMM process per VM, detached.** VMMs outlive whoever started them.
  The daemon restarting, crashing or upgrading never stops a VM. On start it
  finds running VMMs by their sockets in the runtime directory and
  reattaches. On SIGTERM (launchd's stop) or Ctrl-C it stops accepting and
  closes its connections: their sessions end as stopped by the daemon, and
  their recordings are completed before it exits. A second signal exits at
  once.
- **The store and claims.** Bundles on disk are the source of truth, and the
  flock claim is the cross-process lock. The daemon takes claims like any
  other process, so the commands that open the store themselves (`create`,
  `import`, `exec`, `cp`, `record`, `view`, the password and golden commands)
  can't race it.
- **The VMM and agent protocols** (`doppel.vmm.v1`, `doppel.agent.v1`) and
  the fd passing for the agent channel. The daemon is a client of the VMMs.
- **The pure core.** Lifecycle, checkpoint, login and wait state machines,
  AX compaction, refs and captions live in `doppel-machine` and
  `doppel-mcp`; the daemon is a shell around them.

## Shape

```
CLI · doppel mcp (stdio relay) · desktop viewer · browser viewer   local or remote
                    │  doppel.daemon.v1 (protobuf frames)
                    │  unix socket locally · WebSocket over TLS remotely
┌───────────────────▼────────────────────────────────────────────┐
│ doppeld                                                        │
│  machines: lifecycle, claims, per-machine call queues          │
│  sessions: who drives what, takeover, traces, recordings       │
│  streams: display (shared memory locally, H.264 remotely)      │
│  MCP: tool calls run here; `doppel mcp` only relays            │
└──────┬─────────────────────────────────────────────────────────┘
       │ doppel.vmm.v1 over unix sockets
   doppel-vmm × N ──vsock── doppel-agent
```

### Process model

- One daemon per user, started by launchd as a LaunchAgent with socket
  activation: the first client to connect starts it. `doppel daemon
  install` sets it up once; `status`, `stop`, `logs` and `uninstall` manage
  it, and `doppel daemon run` runs it in the foreground.
- Its socket is `doppeld.socket` in the data root, mode 0600: the user's own
  processes only.
- It holds no state that can't be rebuilt from disk and the running VMMs,
  except live sessions (below), which end when it does.
- It is a library run by the `doppel` binary (`doppel daemon run`, which
  launchd invokes), not a second executable: one binary to sign and ship,
  and the version handshake can't skew between them.

### API: `doppel.daemon.v1`

The same framing as the other channels (`[u32 length][protobuf]`, `Hello`
first, unknown messages kept). Its own package, since the VMM speaks for one
VM and the daemon for the host.

| Group      | Requests                                                                                          | Events                                           |
| ---------- | ------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| Machines   | list, status, create, import, start, stop, pause, resume, checkpoint, fork, restore, set password | machine state changed                            |
| Calls      | run a typed tool call, cancel it                                                                  | call started, call finished (outcome, certainty) |
| Sessions   | open, list, attach, pause, take control, hand back, close                                         | session state changed                            |
| Display    | open a stream (shared memory or encoded), host input                                              | frames                                           |
| Recordings | start, stop, list, open a recording and its trace                                                 | recording started, finished                      |

It is protobuf over a sans-IO codec that Rust and Swift produce
byte-identically, pinned by golden fixtures. A browser needs only a transport
it can open, and WebSocket carries binary frames unchanged, so there is no
second framing (gRPC) or second schema (JSON-RPC).

### MCP

The tool runner lives in the daemon: the queues, agent sessions and refs.
`doppel mcp` is the command MCP clients launch (`claude mcp add doppel --
doppel mcp --machine dev`); it serves rmcp over stdio and forwards each typed
`ToolCall` to the daemon. Parsing stays in `doppel-mcp`, at the edge, so the
daemon never sees JSON.

[mcp.md](mcp.md) describes the tools. Each `doppel mcp` process is one
**session** in the daemon, which is what
makes agents visible, recordable and interruptible from elsewhere.

### Sessions and takeover

A session is one client driving machines: an agent through `doppel mcp`, or a
person through a viewer. Other clients observe it, so it is an enum state
machine with pure transitions:

```
Active ──pause──▶ Paused ──resume──▶ Active
  │                 │
  │      take control (viewer)
  │                 ▼
  │            HumanControl ──hand back──▶ Active
  ▼
Ended { Closed | Disconnected | DaemonStopped }
```

- **Paused:** the session's next call waits in its machine's queue; a call
  already running finishes, or is cancelled.
- **HumanControl:** the viewer's input goes to the machine, and the agents'
  calls wait. On hand back, the agent's next result notes that a person had
  control, so it knows to look again before relying on an earlier snapshot.
- **Ended:** recordings complete, and the trace is closed. A daemon that
  stops ends its sessions as `DaemonStopped`, never silently as `Closed`.

### Display streams

- **Local:** the daemon opens the VMM's display and passes the shared-memory
  fd and the slot events to the viewer. No copy and no encode.
- **Remote:** the VMM encodes with a low-latency `VTCompressionSession` (no
  B-frames, real-time, a key frame whenever a viewer joins or falls behind),
  and the daemon forwards the frames. The desktop viewer decodes with
  `VTDecompressionSession`, the browser with WebCodecs. One encoder per VMM
  feeds remote viewers and recordings alike.

Input from a remote viewer is `doppel.daemon.v1` host input; viewer
positions are fractions of the screen.

### Recordings and traces

`doppel mcp --record` (or `fork-run --record`) records its session. Both
viewers replay a recording beside its trace, the call under the playhead
highlighted. `doppel record start NAME FILE.mp4` records one machine's
display to a file, outside any session.

`doppel record export RECORDING OUT.mp4` writes one of a recording's videos
(the first, or `--video NAME`) with what its agent did burned in: the
pointer gliding to where it acts and what it's doing, the marks its calls
leave, its marks for the people watching, and a chip while it runs a
command or moves a file. `doppel_scene::film` decides what is drawn at each
frame, as shapes in display points; `doppel-vmm export` paints them on the
frames and encodes the result, at `--fps` (30 by default). It doesn't yet
draw which elements the agent reads.

A recorded session is a folder in the data directory's `recordings/`, named
for when it started and its session: `2026-11-13T21-04-05Z-s3/`. It holds:

| File                  | What                                                                                                                                                                                                                           |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `{machine}-{NNN}.mp4` | One video per run of each machine the session's calls touch: `dev-001.mp4`, then `dev-002.mp4` after a stop and start or a rollback. H.264 in fragmented MP4, so a file cut short by a crash plays up to its last 2 s fragment |
| `{machine}-{NNN}.vtt` | WebVTT captions beside each video: a cue per call while it ran, and per stretch a person had control                                                                                                                           |
| `trace.jsonl`         | A line per call, in the order they ended                                                                                                                                                                                       |
| `segments.jsonl`      | A line per video as it starts, to line the videos up with the trace                                                                                                                                                            |
| `refs.jsonl`          | What the session named the elements of each snapshot the trace kept                                                                                                                                                            |

Times are milliseconds from the session's start. Readers must allow
`segments.jsonl` and `refs.jsonl` to be missing, any optional field to be
absent, and names they don't know.

**`trace.jsonl`:**

```text
{"start_ms":1500,"end_ms":2250,"machine":"dev","call":"clicked (120, 40)","outcome":"done","gesture":"click","target":{"point":{"x":120.0,"y":40.0}}}
```

- `start_ms`, `end_ms`: when the call ran on its machine.
- `machine`: the machine it named, or `null`.
- `call`: its caption, as the viewers show it.
- `outcome`: `done`, `failed`, `timed_out` (a wait that ran out of time) or
  `refused` (a request that was wrong); `error` says why, when there's a
  reason.
- `waited_ms`: how long it waited while a person had the machine.
- `queued_ms`: how long it waited behind other calls on the machine.
- `by`: `"person"` for a stretch a person had control (`call` is `"a person
  had control"`); absent for the session's own calls.
- `gesture`: `click`, `double_click`, `secondary_click`, `move`, `drag`,
  `scroll`, `key`, `type`, `act`, `look`, `wait`, `run`, `lifecycle`,
  `mark` or `other`.
- `target`: where it acted, in display points, with one key naming the
  kind: `{"point":{"x","y"}}`, `{"element":{"x","y","width","height","label"}}`
  or `{"drag":{"from":{…},"to":{…}}}`.
- `seen`, `read`, `mark`: what it saw, read or marked, as base64 of the
  protobuf message: `doppel.agent.v1.AxSnapshot`, `doppel.daemon.v1.TextRead`
  and `doppel.daemon.v1.ScreenMark`. A snapshot keeps no password field's value.

Captions and the trace name what a call did but leave out what may be
secret: typed text, values set, `exec` arguments and file contents.

**`segments.jsonl`:**
`{"file":"dev-001.mp4","machine":"dev","start_ms":0,"points":{"width":1280.0,"height":800.0}}`,
where `points` is the display's size, to place targets on the video.

**`refs.jsonl`:**
`{"line":4,"snapshot":12,"generation":3,"nodes":[3,7,9]}`: the names the
agent's model was given for the snapshot kept by trace line `line` (from 0).
Ref `s3e2` is element 2 of generation 3: node `nodes[1]` of the snapshot, in
its pre-order. These are the fields of `doppel.daemon.v1.ElementRefs`.

## Remote access

- **SSH:** `ssh -L` forwards the daemon's socket from another Mac, and the
  CLI and the desktop viewer take `--host` to use it. SSH authenticates and
  encrypts. Shared memory can't cross the link, so remote clients get the
  encoded display.
- **The browser viewer:** `doppel daemon web on` serves it over HTTPS and a
  WebSocket, with a self-signed certificate (`doppel daemon web status`
  prints its fingerprint). Only holders of a token get in: `doppel daemon
  token` issues one, short-lived and revocable, printed once; the daemon
  keeps only its digest. It never listens without authentication.

## Roles and lineage

A machine records what it is for and where it came from, in `lineage.pb` in
its bundle: one `doppel.bundle.v1.Lineage` message, pinned as
`fixtures/bundle/lineage.v1.bin`. The machine list carries the same fields in
its own message (`doppel.daemon.v1.Lineage`, in `MachineEntry.lineage`), so
the protocol can change without changing bundles. The pure types are
`doppel-machine::lineage`: `Role::{Working, Golden { template }}`,
`Origin::{Unrecorded, Installed, Imported, Forked { from, checkpoint }}`, and
`RecordedLineage::{Known, Unreadable}` for what a read found.

- **Role.** A golden image is a template: new machines start from its
  `template` checkpoint. `doppel golden prepare` records it when its
  checkpoint completes synced; `doppel golden mark NAME [--checkpoint
  ready]` records it for an existing machine, refusing a checkpoint that
  doesn't exist; `doppel golden unmark NAME` makes it a working machine
  again. Both take the machine's claim and rewrite the file whole (a
  temporary file, then a rename), keeping the origin. They open the store
  themselves; the daemon reads the file on each list.
- **Origin.** Written once, by what made the machine: `create` writes
  `Installed` as it makes the bundle; `import`, `fork` and fresh forks
  (`fork-run`, a viewer's new copy) write `Imported` or `Forked { from,
  checkpoint }` into the staging directory before the rename that makes the
  bundle appear, so a machine never appears without it. A fork is a working
  machine whatever its source's role. `from` names the source as it was; it
  may since have been deleted.
- **Unrecorded.** A bundle without the file reads as a working machine whose
  origin is `Unrecorded`: unknown, never guessed from its name or its spec.
  `ls` and the viewers show nothing extra for it.
- **Unreadable.** A file that can't be read or decoded, or holds a role or
  origin this build doesn't know, doesn't hide the machine or pass for a
  working one: the list carries `lineage_unreadable` with why, `ls` prints
  it, the viewers say "Lineage unreadable", and `golden mark`/`unmark`
  refuse to overwrite it.
- **The viewers** list golden images in a group of their own, whatever their
  state. A stopped golden image's main action is "New machine from it" (a
  fresh fork, the new-copy dialog with its template chosen); Start is in its
  menu and asks first, since a started golden image drifts from its
  template. The status bar says `Golden image · template ready` or
  `From golden-27 · ready`.

## Decisions

1. **One daemon per user**, not per host. It runs as the user, in their
   login session, with their own store. A per-host daemon would run outside
   any login session, where the VMM's display (drawn by a
   `VZVirtualMachineView`) can't work, and it would be a privileged boundary.
   A fleet Mac runs one service account that logs in automatically, with one
   daemon. The cost: on a Mac where several users run VMs, the daemons don't
   coordinate Apple's two-VM limit, so a third start fails with
   Virtualization's error instead of queueing.
2. **Sessions don't survive a daemon restart.** They end as `DaemonStopped`,
   and clients reconnect into a new session. Resuming one would need
   persisted refs and queues.
3. **Two viewers.** The browser viewer runs on any device and makes sharing
   a run a link; the desktop viewer is the fastest view on the Mac itself,
   from shared memory.
4. **Tokens, not client certificates,** for the browser: pinned client
   certificates would need installing on every device, which phones make
   awkward. The cost: a browser warns about the self-signed certificate
   once per device.
5. **The daemon is blocking,** one thread per connection, with no async
   runtime. Cancellation stops only where a call checks for it, so what it
   leaves behind is written down there; at a handful of clients per Mac,
   threads cost nothing. Serving MCP over HTTP to remote agents would be an
   async edge of its own.

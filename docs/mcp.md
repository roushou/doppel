# MCP

`doppel mcp` serves Doppel's tools to an AI agent over MCP (stdio). Each tool's
arguments and their schema come from the server itself (`tools/list`); this
document is how the tools fit together.

## Setup

```bash
claude mcp add doppel -- /path/to/doppel mcp --machine work
```

- `--machine` (or `DOPPEL_MACHINE`) is the machine a call targets when it
  names none. Every tool takes `machine` to target another.
- `--record` (or `DOPPEL_RECORD=1`) records the session: videos, captions and
  a trace of every call. The format is in [doppeld.md](doppeld.md#recordings-and-traces).
- Each `doppel mcp` process is one session in the daemon. People see it in
  the viewers and `doppel sessions`, by the name the client gives itself.

## Tools

| Group     | Tool         | What it does                                                                         |
| --------- | ------------ | ------------------------------------------------------------------------------------ |
| Look      | `status`     | Run state, and the guest agent's session, display, frontmost app and missing grants  |
|           | `screenshot` | The screen; one image pixel is one display point unless downscaled                   |
|           | `snapshot`   | The UI's accessibility elements as compact text, with refs to act on                 |
|           | `find_text`  | Text on screen by recognition, with a point to click for each line or match          |
| Act       | `act`        | Act on an element by ref: press, select, focus, set a value, open its menu, and more |
|           | `click`      | Click a point, in display points                                                     |
|           | `type`       | Type text into whatever has focus                                                    |
|           | `key`        | Press a key or chord (`cmd+space`, `ctrl+c`)                                         |
| Wait      | `wait_for`   | Poll until an element, a frontmost app or on-screen text appears or disappears       |
| Run       | `exec`       | Run a program in the guest as the logged-in user                                     |
|           | `read_file`  | Read a file in the guest                                                             |
|           | `write_file` | Write a file in the guest, atomically                                                |
| Show      | `mark`       | Point something out to the people watching; the guest never sees it                  |
| Lifecycle | `start`      | Start a stopped machine and wait for its desktop                                     |
|           | `stop`       | Power the machine off at once                                                        |
|           | `checkpoint` | Save memory and disk under a label, and keep running                                 |
|           | `rollback`   | Return to a checkpoint, discarding everything since                                  |
|           | `fork`       | Make a new, stopped machine from a checkpoint                                        |

## Snapshots and refs

`snapshot` lists what's on screen as text within a token budget (4000 by
default), leaving out what can't be read or acted on, unnamed wrappers, and
menus unless asked. Each element an agent can act on carries a ref like
`[s3e12]`: element 12 of the session's third snapshot.

- A new snapshot's refs replace the previous ones on that machine. A ref
  from an older snapshot, or to an element that has gone, is an error: snapshot again.
- `wait_for` with `appears` or `disappears` returns a fresh snapshot, so its
  refs are the current ones.
- After `rollback`, or a person's control (below), earlier refs don't apply.

### Changes only

With `show: "changes"`, `snapshot` and `wait_for` list only what differs
from the machine's previous listing: elements that appeared (`+`), changed
their value or states (`~`, with what they were) or went (`-`, the topmost
of what went, with how many went inside it). Elements are matched by their
place in the tree, role and name, so a renamed element went and another
appeared.

- What appeared or changed gets refs of the new listing. Everything else is
  as it was, and the refs the agent holds for it still resolve.
- It lists everything instead, saying why, when there's no previous listing
  of the machine, another app is frontmost, or most of the UI is new.
- The viewers light up what the agent was shown, so a listing of changes
  shows the people watching what the act did.

Accessibility covers native apps and Electron apps; Electron's first snapshot
after it starts can take a few seconds. Chrome shows page content only when
started with `--force-renderer-accessibility`, for example through `exec`:
`/usr/bin/open -a "Google Chrome" --args --force-renderer-accessibility URL`.
For apps with no useful tree, use `find_text` and `screenshot`.

## Two paths for input and screen

- **The guest agent** runs in the logged-in session. `snapshot`, `act`,
  `exec` and the file tools need it, and input goes through it by default.
- **The host** reaches the VM's own display and devices. `screenshot` and
  `find_text` use it by default, and input takes it with `via: "host"`. It
  works before login, while locked, and without the agent.

## Acting, then waiting

Input and the screen are asynchronous: a call returns when its input is
sent, not when the screen shows its effect. Don't sleep; wait on evidence
with `wait_for`, which polls until its condition holds or its timeout (5 s by
default, at most 60 s) runs out, and then says what it last saw.

## Outcomes

- A call succeeds, or fails with an error the model can read.
- A failure can be **unknown**: a timeout or a lost connection, where the
  call may have taken effect. The result says so; check with `status`,
  `snapshot` or `screenshot` before retrying.
- Calls on one machine run one at a time, across every session; a call waits
  its turn.
- Cancelling a call (the client's `notifications/cancelled`) gives up its
  place, ends a wait, and stops an `exec`'s program.

## People

People watch sessions in the viewers, and can pause a session or take
control of a machine. While a person has control, calls on that machine
wait. When they hand back, the session's next result on that machine ends
with a note that a person had control, so the agent snapshots again before
using earlier refs.

`mark` is the other direction: it draws a box, dot or arrow with a short note
over the screen for the people watching, and recordings keep it.

Every tool also takes an optional `why`: one line, at most 120 characters, on
why the agent makes the call ("to check the signup form"). The viewers show
it under the pointer's caption, and the trace, the recording's captions and
exports keep it. It's the agent's own words, so it goes wherever the
recording goes: keep secrets out of it.

## Lifecycle

`stop` and `rollback` discard the running state: open apps and unsaved work
are lost. `checkpoint` first to keep it. A rollback starts the machine shown
in a window if it was (`doppel start --window`), hidden otherwise.

Right after a machine resumes from saved memory (a `start` that resumes, a
`rollback`, or the end of a `checkpoint`), its screen can lag behind its
apps for a few seconds: Virtualization.framework rebuilds the guest's
graphics, and the guest's drawing waits on it while everything else runs.
Accessibility answers at once; the screen, screenshots, `find_text` and
recordings catch up when drawing does. A window on the host doesn't avoid
it. `wait_for` on what you expect to see, rather than reading the screen
straight after these calls. A fork shares its source's identity,
so it can't run while its source or another fork of it runs, and Apple runs
at most two macOS machines at once. There's no tool to delete a checkpoint: they
are the person's to keep, and a deletion can't be undone. People use `doppel
forget-checkpoint`.

## What's never recorded

Captions, traces and the viewers name what a call did, but leave out typed
text, values set, `exec` arguments and file contents. Snapshots shown to
people keep no password field's value.

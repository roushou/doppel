// What an agent is doing on a machine, as a scene drawn over its screen:
// where its pointer is, the call under way, and the marks recent calls left
// (a ripple where it clicked, a box round what it pressed, the path of a
// drag). Pure: the daemon's activity read into calls, and how a call reads;
// `timeline.ts` works out the scene at an instant.

import {
  type Activity,
  type CallEnded,
  type CallStarted,
  type ScreenMark,
  Gesture as WireGesture,
  MarkShape,
} from "../gen/doppel/daemon/v1/daemon_pb";
import type { DisplayPoint, Rect } from "./geometry";
import type { MachineName } from "./machines";
import { type TextRead, readText } from "./reading";
import { type Refs, type Seen, readRefs, readSnapshot } from "./tree";

export type Gesture =
  | "click"
  | "double-click"
  | "secondary-click"
  | "move"
  | "drag"
  | "scroll"
  | "key"
  | "type"
  | "act"
  | "look"
  | "wait"
  | "run"
  | "lifecycle"
  | "checkpoint"
  | "rollback"
  | "mark"
  | "other";

/**
 * A mark an agent put on the screen for the people watching. It has its own
 * place: the agent's pointer doesn't go there.
 */
export interface Callout {
  on: { kind: "point"; at: DisplayPoint } | { kind: "area"; frame: Rect; label: string };
  shape: { kind: "box" } | { kind: "dot" } | { kind: "arrow"; from: DisplayPoint | null };
  /** One short line. */
  note: string | null;
  /** How long it stays, in ms; until the agent next changes the screen too, when `untilActed`. */
  lasts: { kind: "for"; ms: number } | { kind: "until-acted"; ms: number };
}

/** Where a call acts, in display points. */
export type Target =
  | { kind: "point"; at: DisplayPoint }
  | { kind: "element"; frame: Rect; label: string }
  | { kind: "drag"; from: DisplayPoint; to: DisplayPoint }
  | { kind: "none" };

/** A call of an agent's session, as it started. */
export interface Call {
  /** Its session and request: a start and its end share it. */
  key: string;
  /** Its session's id; 0 for a recording's one session. */
  session: number;
  machine: MachineName;
  client: string;
  caption: string;
  gesture: Gesture;
  target: Target;
  /** What a mark call shows. */
  callout: Callout | null;
  /** Why the session's client made it, in its words, when it said. */
  why: string | null;
}

export type Ending = { kind: "done" } | { kind: "failed"; reason: string };

export type ActivityEvent =
  | { kind: "started"; call: Call }
  | { kind: "ended"; key: string; machine: MachineName; ending: Ending; seen: Seen | null; read: TextRead | null }
  /** A call from before the page began watching: how long before it started and, unless still under way, ended. */
  | {
      kind: "recent";
      call: Call;
      startedAgoMs: number;
      ended: { ending: Ending; seen: Seen | null; read: TextRead | null; refs: Refs | null; agoMs: number } | null;
    }
  /** The agent named the elements of a snapshot one of its calls saw, as its model was shown them. */
  | { kind: "named"; machine: MachineName; refs: Refs };

function gesture(wire: WireGesture): Gesture {
  switch (wire) {
    case WireGesture.CLICK:
      return "click";
    case WireGesture.DOUBLE_CLICK:
      return "double-click";
    case WireGesture.SECONDARY_CLICK:
      return "secondary-click";
    case WireGesture.MOVE:
      return "move";
    case WireGesture.DRAG:
      return "drag";
    case WireGesture.SCROLL:
      return "scroll";
    case WireGesture.KEY:
      return "key";
    case WireGesture.TYPE:
      return "type";
    case WireGesture.ACT:
      return "act";
    case WireGesture.LOOK:
      return "look";
    case WireGesture.WAIT:
      return "wait";
    case WireGesture.RUN:
      return "run";
    case WireGesture.LIFECYCLE:
      return "lifecycle";
    case WireGesture.CHECKPOINT:
      return "checkpoint";
    case WireGesture.ROLLBACK:
      return "rollback";
    case WireGesture.MARK:
      return "mark";
    case WireGesture.OTHER:
    case WireGesture.UNSPECIFIED:
      return "other";
    default:
      // A gesture a newer daemon added.
      return "other";
  }
}

function point(at: { x: number; y: number } | undefined): DisplayPoint | null {
  return at === undefined || !Number.isFinite(at.x) || !Number.isFinite(at.y) ? null : { x: at.x, y: at.y };
}

/** How long a mark stays when the daemon doesn't say. */
const CALLOUT_MS = 5000;

/** A mark as the daemon told it; `null` for one without a place this page can read. */
export function readCallout(mark: ScreenMark): Callout | null {
  let on: Callout["on"] | null = null;
  const at = mark.at;
  switch (at.case) {
    case "point": {
      const point_ = point(at.value);
      if (point_ !== null) on = { kind: "point", at: point_ };
      break;
    }
    case "area": {
      const origin = point(at.value.frame?.origin);
      const size = at.value.frame?.size;
      if (origin !== null && size !== undefined && size.width > 0 && size.height > 0) {
        on = { kind: "area", frame: { left: origin.x, top: origin.y, width: size.width, height: size.height }, label: at.value.label };
      }
      break;
    }
    case undefined:
      break;
    default: {
      const never: never = at;
      return never;
    }
  }
  if (on === null) return null;
  let shape: Callout["shape"];
  switch (mark.shape) {
    case MarkShape.DOT:
      shape = { kind: "dot" };
      break;
    case MarkShape.ARROW:
      shape = { kind: "arrow", from: point(mark.from) };
      break;
    case MarkShape.BOX:
    case MarkShape.UNSPECIFIED:
    default:
      // A shape a newer daemon added is drawn as a box.
      shape = { kind: "box" };
  }
  const lasts = mark.lasts;
  let lasting: Callout["lasts"];
  switch (lasts.case) {
    case "forMs":
      lasting = { kind: "for", ms: lasts.value };
      break;
    case "untilActedMs":
      lasting = { kind: "until-acted", ms: lasts.value };
      break;
    case undefined:
      lasting = { kind: "for", ms: CALLOUT_MS };
      break;
    default: {
      const never: never = lasts;
      return never;
    }
  }
  const note = mark.note.trim();
  return { on, shape, note: note === "" ? null : note, lasts: lasting };
}

/** What a call did on the screen and where, as the daemon told it. A mark's place is its own: it has no target. */
export function parseStarted(started: CallStarted): { gesture: Gesture; target: Target; callout: Callout | null; why: string | null } {
  const said = started.why.trim();
  const why = said === "" ? null : said;
  if (started.mark !== undefined) {
    return { gesture: gesture(started.gesture), target: { kind: "none" }, callout: readCallout(started.mark), why };
  }
  const wire = started.target;
  let target: Target = { kind: "none" };
  switch (wire.case) {
    case "point": {
      const at = point(wire.value);
      if (at !== null) target = { kind: "point", at };
      break;
    }
    case "element": {
      const origin = point(wire.value.frame?.origin);
      const size = wire.value.frame?.size;
      if (origin !== null && size !== undefined && size.width > 0 && size.height > 0) {
        target = {
          kind: "element",
          frame: { left: origin.x, top: origin.y, width: size.width, height: size.height },
          label: wire.value.label,
        };
      }
      break;
    }
    case "drag": {
      const from = point(wire.value.from);
      const to = point(wire.value.to);
      if (from !== null && to !== null) target = { kind: "drag", from, to };
      break;
    }
    case undefined:
      break;
    default: {
      const never: never = wire;
      return never;
    }
  }
  return { gesture: gesture(started.gesture), target, callout: null, why };
}

/** How a call ended, the snapshot it saw, and the text it read. */
function endingOf(ended: CallEnded): { ending: Ending; seen: Seen | null; read: TextRead | null } {
  const seen = ended.seen === undefined ? null : readSnapshot(ended.seen);
  const read = ended.read === undefined ? null : readText(ended.read);
  const outcome = ended.outcome;
  switch (outcome.case) {
    case "done":
      return { ending: { kind: "done" }, seen, read };
    case "failed":
    case "refused":
      return { ending: { kind: "failed", reason: outcome.value }, seen, read };
    case "timedOut":
      return {
        ending: { kind: "failed", reason: outcome.value === "" ? "timed out" : `timed out: ${outcome.value}` },
        seen,
        read,
      };
    case undefined:
      // An ending this page can't read still ends the call.
      return { ending: { kind: "done" }, seen, read };
    default: {
      const never: never = outcome;
      return never;
    }
  }
}

/** The daemon's activity as an event the timeline takes; `null` for one this page can't read. */
export function parseActivity(activity: Activity): ActivityEvent | null {
  const key = `${activity.session}:${activity.request}`;
  const machine = activity.machine as MachineName;
  const phase = activity.phase;
  switch (phase.case) {
    case "started": {
      const { gesture: acted, target, callout, why } = parseStarted(phase.value);
      return {
        kind: "started",
        call: {
          key,
          session: Number(activity.session),
          machine,
          client: activity.client,
          caption: activity.caption,
          gesture: acted,
          target,
          callout,
          why,
        },
      };
    }
    case "ended": {
      const { ending, seen, read } = endingOf(phase.value);
      return { kind: "ended", key, machine, ending, seen, read };
    }
    case "recent": {
      const recent = phase.value;
      if (recent.started === undefined) return null;
      const { gesture: acted, target, callout, why } = parseStarted(recent.started);
      const ended =
        recent.ended === undefined || recent.endedMsAgo === undefined
          ? null
          : {
              ...endingOf(recent.ended),
              refs: recent.ended.refs === undefined ? null : readRefs(recent.ended.refs),
              agoMs: Number(recent.endedMsAgo),
            };
      return {
        kind: "recent",
        call: { key, session: Number(activity.session), machine, client: activity.client, caption: activity.caption, gesture: acted, target, callout, why },
        startedAgoMs: Number(recent.startedMsAgo),
        ended,
      };
    }
    case "named": {
      const refs = readRefs(phase.value);
      return refs === null ? null : { kind: "named", machine, refs };
    }
    case undefined:
      return null;
    default: {
      const never: never = phase;
      return never;
    }
  }
}

/**
 * A mark a call left on the screen, showing from `since` until `until`: a
 * frame over an element or a path just after its call ends, or when its
 * agent's next call starts; a ripple for `MARK_MS`.
 */
export type Mark =
  | { id: number; kind: "ripple"; at: DisplayPoint; gesture: Gesture; since: number; until: number }
  | { id: number; kind: "box"; frame: Rect; label: string; since: number; until: number }
  | { id: number; kind: "path"; from: DisplayPoint; to: DisplayPoint; since: number; until: number }
  | { id: number; kind: "failed"; at: DisplayPoint; reason: string; since: number; until: number };

export const MARK_MS = 1600;
/** How long a mark shows after its call ends: the screen changes then. */
export const SETTLE_MS = 150;

/** How long the reading band takes to sweep down the screen once. */
export const SWEEP_MS = 1300;

/**
 * When the band of a look from `start` to `end` (null: still looking)
 * leaves the screen: at the end of the pass under way when the look ends,
 * so it never stops midway, and after one pass at the least.
 */
export function sweepEnds(start: number, end: number | null): number {
  if (end === null) return Infinity;
  return start + Math.max(1, Math.ceil((end - start) / SWEEP_MS)) * SWEEP_MS;
}

/** One machine's scene: each agent's, and the marks agents put on the screen. */
export interface Scene {
  /** Each session that has acted on the machine, by session id. */
  agents: AgentScene[];
  /** The marks agents put on the screen that are showing. */
  callouts: Shown[];
}

/** One session's part of a machine's scene. */
export interface AgentScene {
  session: number;
  /** Its client, in its own words. */
  client: string;
  /** Where its pointer is: the last place it acted. */
  cursor: DisplayPoint | null;
  /** Its call under way, and since when. */
  current: { call: Call; since: number } | null;
  /** When the look whose band is sweeping the screen began, if one is. */
  scan: number | null;
  marks: Mark[];
  /** When it last did anything: its pointer fades after a while. */
  active: number;
}

/** A mark an agent put on the screen, showing from `since` until `until`. */
export interface Shown {
  id: string;
  callout: Callout;
  since: number;
  until: number;
}

/** Where a target puts the pointer: a point, an element's centre, a drag's end. */
export function aim(target: Target): DisplayPoint | null {
  switch (target.kind) {
    case "point":
      return target.at;
    case "element":
      return { x: target.frame.left + target.frame.width / 2, y: target.frame.top + target.frame.height / 2 };
    case "drag":
      return target.to;
    case "none":
      return null;
    default: {
      const never: never = target;
      return never;
    }
  }
}

/** What the call under way reads as, for a person. */
export function describe(call: Call): string {
  const named = call.target.kind === "element" && call.target.label !== "" ? `“${call.target.label}”` : null;
  switch (call.gesture) {
    case "click":
      return named ?? "Clicking";
    case "double-click":
      return "Double-clicking";
    case "secondary-click":
      return "Right-clicking";
    case "move":
      return "Moving the pointer";
    case "drag":
      return "Dragging";
    case "scroll":
      return "Scrolling";
    case "key":
      return "Pressing a key";
    case "type":
      return "Typing";
    case "act":
      // A press, a value set, a focus: the element is what matters.
      return named ?? "Using an element";
    case "look":
      return "Looking at the screen";
    case "wait":
      return "Waiting";
    case "run":
      return "Running a command";
    case "mark":
      return call.callout?.note == null ? "Pointing something out" : `Pointing out: ${call.callout.note}`;
    case "checkpoint":
      return "Keeping a checkpoint";
    case "rollback":
      return "Rolling back to a checkpoint";
    case "lifecycle":
      return call.caption === "" ? "Changing the machine" : call.caption;
    case "other":
      return call.caption;
    default: {
      const never: never = call.gesture;
      return never;
    }
  }
}

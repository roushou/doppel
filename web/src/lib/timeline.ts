// One timeline for a machine, live or recorded: the calls on it as moments
// (when each started and ended, what it did and where), and the scene the
// overlay draws at any instant, worked out from them. Live, the moments come
// from the activity stream, timed by the page's clock; in a replay, from the
// recording's trace, timed from the session's start. Pure.

import type { Call as TraceCall } from "./recordings";
import { type ActivityEvent, type AgentScene, type Call, type Gesture, type Mark, MARK_MS, type Scene, type Shown, type Target, aim, sweepEnds } from "./overlay";
import type { TextRead } from "./reading";
import type { Refs, Seen } from "./tree";

/** Where the snapshot a call saw is: with the call, live, or on a recording's trace line, to fetch. */
export type SeenSource = { kind: "live"; seen: Seen } | { kind: "recorded"; line: number };

/** A call on the timeline. */
export interface Moment {
  key: string;
  startMs: number;
  /** `null` while it's under way. */
  endMs: number | null;
  call: Call;
  /** Why it didn't succeed, if it didn't. */
  failed: string | null;
  /** A person who had control, not the agent. */
  person: boolean;
  /** The accessibility snapshot it saw, if it took one. */
  seen: SeenSource | null;
  /** What the agent named that snapshot's elements, once it has. */
  refs: Refs | null;
  /** What it read off the screen, if it read it. */
  read: TextRead | null;
}

/** How many moments a live timeline keeps for each machine. */
export const KEPT = 300;

/** The moments after a live activity event at `now`, the oldest dropped past `KEPT`. */
export function record(moments: readonly Moment[], event: ActivityEvent, now: number): readonly Moment[] {
  switch (event.kind) {
    case "started": {
      const started: Moment = {
        key: event.call.key,
        startMs: now,
        endMs: null,
        call: event.call,
        failed: null,
        person: false,
        seen: null,
        refs: null,
        read: null,
      };
      const next = [...moments, started];
      return next.length > KEPT ? next.slice(next.length - KEPT) : next;
    }
    case "ended":
      return moments.map((moment) =>
        moment.key === event.key && moment.endMs === null
          ? {
              ...moment,
              endMs: now,
              failed: event.ending.kind === "failed" ? event.ending.reason : null,
              seen: event.seen === null ? null : { kind: "live", seen: event.seen },
              read: event.read,
            }
          : moment,
      );
    case "recent": {
      // Told once, when watching begins; a call already here stays as it is.
      if (moments.some((moment) => moment.key === event.call.key)) return moments;
      const placed: Moment = {
        key: event.call.key,
        startMs: now - event.startedAgoMs,
        endMs: event.ended === null ? null : now - event.ended.agoMs,
        call: event.call,
        failed: event.ended?.ending.kind === "failed" ? event.ended.ending.reason : null,
        person: false,
        seen: event.ended?.seen == null ? null : { kind: "live", seen: event.ended.seen },
        refs: event.ended?.refs ?? null,
        read: event.ended?.read ?? null,
      };
      const next = [...moments, placed].sort((a, b) => a.startMs - b.startMs);
      return next.length > KEPT ? next.slice(next.length - KEPT) : next;
    }
    case "named": {
      // The latest call that saw the snapshot: the agent may reuse an id once it restarts.
      let saw = -1;
      moments.forEach((moment, index) => {
        if (moment.seen?.kind === "live" && moment.seen.seen.snapshot === event.refs.snapshot) saw = index;
      });
      return saw === -1 ? moments : moments.map((moment, index) => (index === saw ? { ...moment, refs: event.refs } : moment));
    }
    default: {
      const never: never = event;
      return never;
    }
  }
}

/** A recording's calls on `machine` (and those naming none) as moments, in start order. */
export function fromTrace(trace: TraceCall[], machine: string): Moment[] {
  return trace
    .filter((call) => call.machine === null || call.machine === machine)
    .map((traced, index) => {
      const failed = traced.outcome === "done" ? null : (traced.error ?? outcomeWords(traced.outcome));
      const call: Call = {
        key: `trace:${index}`,
        session: 0,
        machine: machine as Call["machine"],
        client: "",
        caption: traced.call,
        gesture: traced.acted?.gesture ?? "other",
        target: traced.acted?.target ?? { kind: "none" },
        callout: traced.acted?.callout ?? null,
      };
      return {
        key: call.key,
        startMs: traced.startMs,
        endMs: traced.endMs,
        call,
        failed,
        person: traced.by === "person",
        seen: traced.seen === null ? null : { kind: "recorded", line: traced.seen },
        refs: traced.refs,
        read: traced.read,
      };
    });
}

function outcomeWords(outcome: TraceCall["outcome"]): string {
  if (typeof outcome === "object") return `outcome ${outcome.unknown}`;
  switch (outcome) {
    case "done":
      return "done";
    case "failed":
      return "failed";
    case "timed_out":
      return "timed out";
    case "refused":
      return "refused";
    default: {
      const never: never = outcome;
      return never;
    }
  }
}

/** The mark a call leaves where it acted, if it leaves one. */
function leaves(call: Call, id: number, since: number): Mark | null {
  const target: Target = call.target;
  switch (target.kind) {
    case "element":
      return { id, kind: "box", frame: target.frame, label: target.label, since };
    case "drag":
      return { id, kind: "path", from: target.from, to: target.to, since };
    case "point":
      return pointMark(call.gesture) ? { id, kind: "ripple", at: target.at, gesture: call.gesture, since } : null;
    case "none":
      return null;
    default: {
      const never: never = target;
      return never;
    }
  }
}

function pointMark(gesture: Gesture): boolean {
  switch (gesture) {
    case "click":
    case "double-click":
    case "secondary-click":
    case "scroll":
      return true;
    case "move":
    case "drag":
    case "key":
    case "type":
    case "act":
    case "look":
    case "wait":
    case "run":
    case "lifecycle":
    case "mark":
    case "other":
      return false;
    default: {
      const never: never = gesture;
      return never;
    }
  }
}

/**
 * The agents' marks showing at `at`, oldest first. A mark goes after its
 * time or, for one that lasts until the agent acts, when the next call that
 * may change the screen starts. A failed mark call shows nothing.
 */
export function calloutsAt(moments: readonly Moment[], at: number): Shown[] {
  const shown: Shown[] = [];
  moments.forEach((moment, index) => {
    const callout = moment.call.callout;
    if (callout === null || moment.person || moment.failed !== null || moment.startMs > at) return;
    let until = moment.startMs + callout.lasts.ms;
    if (callout.lasts.kind === "until-acted") {
      for (const later of moments.slice(index + 1)) {
        if (later.startMs >= moment.startMs && changes(later)) until = Math.min(until, later.startMs);
      }
    }
    if (at < until) shown.push({ id: moment.key, callout, since: moment.startMs, until });
  });
  return shown;
}

/** A call too quick to see is shown under way for at least this long. */
export const MIN_UNDER_WAY_MS = 600;

/**
 * The scene at `at`: the pointer where the latest call that had a place
 * acted, the call under way (the latest one started), and the marks of
 * calls that started or failed less than `MARK_MS` before. A person's
 * stretches of control draw nothing: they saw their own pointer.
 */
export function sceneAt(moments: readonly Moment[], at: number): Scene {
  const agents = new Map<number, AgentScene>();
  moments.forEach((moment, index) => {
    if (moment.person || moment.startMs > at) return;
    const session = moment.call.session;
    let agent = agents.get(session);
    if (agent === undefined) {
      agent = { session, client: moment.call.client, cursor: null, aimed: null, current: null, scan: null, marks: [], active: -Infinity };
      agents.set(session, agent);
    }
    const placed = aim(moment.call.target);
    if (placed !== null) {
      agent.cursor = placed;
      agent.aimed = moment.startMs;
    }
    agent.active = Math.max(agent.active, moment.startMs);
    const shownUntil = moment.endMs === null ? Infinity : Math.max(moment.endMs, moment.startMs + MIN_UNDER_WAY_MS);
    if (at < shownUntil) agent.current = { call: moment.call, since: moment.startMs };
    if (moment.call.gesture === "look" && at < sweepEnds(moment.startMs, moment.endMs)) agent.scan = moment.startMs;
    if (at - moment.startMs < MARK_MS) {
      const mark = leaves(moment.call, index * 2, moment.startMs);
      if (mark !== null) agent.marks.push(mark);
    }
    if (moment.endMs !== null) {
      agent.active = Math.max(agent.active, Math.min(moment.endMs, at));
      if (moment.failed !== null && moment.endMs <= at && at - moment.endMs < MARK_MS && agent.cursor !== null) {
        agent.marks.push({ id: index * 2 + 1, kind: "failed", at: agent.cursor, reason: moment.failed, since: moment.endMs });
      }
    }
  });
  return { agents: [...agents.values()].sort((a, b) => a.session - b.session), callouts: calloutsAt(moments, at) };
}

/** One row of a timeline strip: a session's calls, with who made them. */
export interface Lane {
  session: number;
  client: string;
  moments: Moment[];
}

/** The calls in a strip, a lane per session, by session id. A person's stretches of control stay in the lane of the session they took over. */
export function lanes(moments: readonly Moment[]): Lane[] {
  const lanes = new Map<number, Lane>();
  for (const moment of moments) {
    const lane = lanes.get(moment.call.session);
    if (lane === undefined) lanes.set(moment.call.session, { session: moment.call.session, client: moment.call.client, moments: [moment] });
    else {
      if (lane.client === "") lane.client = moment.call.client;
      lane.moments.push(moment);
    }
  }
  return [...lanes.values()].sort((a, b) => a.session - b.session);
}

/** The moments that show between `from` and `to`: those overlapping it. */
export function within(moments: readonly Moment[], from: number, to: number, now: number): Moment[] {
  return moments.filter((moment) => moment.startMs <= to && (moment.endMs ?? now) >= from);
}

/** The colour family a gesture is shown in on the timeline. */
export type Family = "pointer" | "keys" | "look" | "wait" | "run" | "machine" | "mark" | "other";

export function family(gesture: Gesture): Family {
  switch (gesture) {
    case "click":
    case "double-click":
    case "secondary-click":
    case "move":
    case "drag":
    case "scroll":
    case "act":
      return "pointer";
    case "key":
    case "type":
      return "keys";
    case "look":
      return "look";
    case "wait":
      return "wait";
    case "run":
      return "run";
    case "lifecycle":
      return "machine";
    case "mark":
      return "mark";
    case "other":
      return "other";
    default: {
      const never: never = gesture;
      return never;
    }
  }
}

/** The snapshot shown at an instant: which call took it, and whether the screen was acted on since. */
export interface SeenAt {
  moment: Moment;
  source: SeenSource;
  /** When it was taken: its call's end. */
  sinceMs: number;
  /** A later call (or a person) acted on the machine before `at`: the screen may have changed. */
  actedSince: boolean;
}

/** The latest snapshot a call had seen by `at`, or `null`. */
export function seenAt(moments: readonly Moment[], at: number): SeenAt | null {
  let found: { moment: Moment; source: SeenSource; sinceMs: number } | null = null;
  for (const moment of moments) {
    if (moment.seen === null || moment.endMs === null || moment.endMs > at) continue;
    if (found === null || moment.endMs >= found.sinceMs) found = { moment, source: moment.seen, sinceMs: moment.endMs };
  }
  if (found === null) return null;
  const taken = found;
  const actedSince = moments.some(
    (moment) => moment !== taken.moment && moment.startMs >= taken.moment.startMs && moment.startMs <= at && changes(moment),
  );
  return { ...taken, actedSince };
}

/** The latest text a call had read by `at`: when, and whether the screen may have changed since. */
export interface ReadAt {
  read: TextRead;
  sinceMs: number;
  actedSince: boolean;
}

export function readAt(moments: readonly Moment[], at: number): ReadAt | null {
  let found: { moment: Moment; read: TextRead; sinceMs: number } | null = null;
  for (const moment of moments) {
    if (moment.read === null || moment.endMs === null || moment.endMs > at) continue;
    if (found === null || moment.endMs >= found.sinceMs) found = { moment, read: moment.read, sinceMs: moment.endMs };
  }
  if (found === null) return null;
  const taken = found;
  const actedSince = moments.some(
    (moment) => moment !== taken.moment && moment.startMs >= taken.moment.startMs && moment.startMs <= at && changes(moment),
  );
  return { read: taken.read, sinceMs: taken.sinceMs, actedSince };
}

/** A person's own read of a machine's screen text, asked at `atMs`. */
export interface TextLook {
  atMs: number;
  outcome: { kind: "loading" } | { kind: "read"; read: TextRead } | { kind: "failed"; why: string };
}

/** What the text layer shows at `at`: the newer of the agent's latest read and the person's own. */
export type Reading =
  | { kind: "agent"; found: ReadAt }
  | { kind: "person"; look: TextLook; actedSince: boolean }
  | { kind: "nothing" };

export function reading(moments: readonly Moment[], look: TextLook | null, at: number): Reading {
  const found = readAt(moments, at);
  if (look !== null && (found === null || found.sinceMs <= look.atMs)) {
    const actedSince = moments.some((moment) => moment.startMs >= look.atMs && moment.startMs <= at && changes(moment));
    return { kind: "person", look, actedSince };
  }
  return found === null ? { kind: "nothing" } : { kind: "agent", found };
}

/** A person's own look at a machine's elements, asked at `atMs`. */
export interface Look {
  atMs: number;
  outcome: { kind: "loading" } | { kind: "seen"; seen: Seen } | { kind: "failed"; why: string };
}

/** What the elements panel shows at `at`: the newer of the agent's latest snapshot and the person's own look. */
export type Showing =
  | { kind: "agent"; found: SeenAt }
  | { kind: "person"; look: Look; actedSince: boolean }
  | { kind: "nothing" };

export function showing(moments: readonly Moment[], look: Look | null, at: number): Showing {
  const found = seenAt(moments, at);
  if (look !== null && (found === null || found.sinceMs <= look.atMs)) {
    const actedSince = moments.some((moment) => moment.startMs >= look.atMs && moment.startMs <= at && changes(moment));
    return { kind: "person", look, actedSince };
  }
  return found === null ? { kind: "nothing" } : { kind: "agent", found };
}

/** Whether a call may change what's on the screen. */
function changes(moment: Moment): boolean {
  if (moment.person) return true;
  const kind = family(moment.call.gesture);
  switch (kind) {
    case "pointer":
    case "keys":
    case "run":
    case "machine":
      return true;
    case "look":
    case "wait":
    case "mark":
    case "other":
      return false;
    default: {
      const never: never = kind;
      return never;
    }
  }
}

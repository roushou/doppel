// What an agent attends to after a snapshot: the elements its model was
// shown, read one after another in the order it was given them, so the
// people watching see where it looks while it thinks. Pure: the snapshot
// and its refs in, the elements lit at an instant out.

import type { DisplayPoint, Rect } from "./geometry";
import type { AgentScene } from "./overlay";
import type { Moment, SeenAt } from "./timeline";
import type { Element, Kind, Seen } from "./tree";

/** One element the agent reads: its ref, where it is, and when it lights up. */
export interface Glance {
  index: number;
  ref: string;
  /** What's on screen of it, in display points. */
  frame: Rect;
  kind: Kind;
  /** Its ref and what it is, as the model read it (`s3e12 Button “Save”`). */
  label: string;
  /** When it lights up, in the scene's milliseconds. */
  at: number;
}

/** A glance lit at an instant, and how brightly (1 just lit, fading to a floor). */
export interface Lit {
  glance: Glance;
  glow: number;
}

/** What an agent attends to at an instant. */
export interface Attention {
  session: number;
  /** The elements read so far, in reading order. */
  lit: Lit[];
  /** The element read now; `null` once all are read. */
  head: Glance | null;
  /** Where its eyes are: the head's centre, or the last read's once done. */
  gaze: DisplayPoint | null;
  /** How much of it shows: 1 while reading and after, fading once the agent calls again, then 0 (its eyes stay). */
  presence: number;
  /** When the snapshot was taken: a look under way before this is over. */
  since: number;
}

/** At most this many elements are read: past that the eye can't follow. */
export const MOST_GLANCES = 60;
/** Reading a snapshot takes about this long, however many elements it has. */
export const READ_MS = 2400;
/** Each glance takes at least, and at most, this long. */
export const GLANCE_MIN_MS = 40;
export const GLANCE_MAX_MS = 140;
/** A glance fades from full to its floor over this long. */
export const AFTERGLOW_MS = 700;
/** How bright a read element stays once read. */
export const FLOOR = 0.28;
/** It all fades this long after the agent's next call starts. */
export const FADE_MS = 450;

/** Kinds worth reading: what has content or can be acted on. Windows and containers only frame the rest. */
function read(element: Element): boolean {
  switch (element.kind) {
    case "control":
    case "field":
    case "text":
    case "menu":
      return true;
    case "other":
      return element.name !== "";
    case "window":
    case "container":
      return false;
    default: {
      const never: never = element.kind;
      return never;
    }
  }
}

/** Too small to see drawn at a screen's scale. */
function tiny(frame: Rect): boolean {
  return frame.width < 3 || frame.height < 3;
}

/**
 * The glances of a snapshot an agent's model was shown, from `since`: the
 * elements it named, in the order it named them, that are on screen and
 * worth reading. Empty when it named none of them.
 */
export function glances(seen: Seen, named: ReadonlyMap<number, string>, since: number): Glance[] {
  const picked: { element: Element & { seen: Rect }; ref: string }[] = [];
  for (const [index, ref] of named) {
    const element = seen.elements[index];
    if (element === undefined || element.seen === null || element.wrapper || !read(element) || tiny(element.seen)) continue;
    picked.push({ element: { ...element, seen: element.seen }, ref });
    if (picked.length === MOST_GLANCES) break;
  }
  const step = Math.min(GLANCE_MAX_MS, Math.max(GLANCE_MIN_MS, READ_MS / Math.max(1, picked.length)));
  return picked.map(({ element, ref }, position) => ({
    index: element.index,
    ref,
    frame: element.seen,
    kind: element.kind,
    label: element.name === "" ? `${ref} ${element.roleWords}` : `${ref} ${element.roleWords} “${element.name}”`,
    at: since + position * step,
  }));
}

function centre(frame: Rect): DisplayPoint {
  return { x: frame.left + frame.width / 2, y: frame.top + frame.height / 2 };
}

/**
 * What the agent that took `found`'s snapshot attends to at `at`: its
 * glances lit so far, until it calls again, then fading; once faded,
 * none lit but its eyes where they last were. `null` when it named none
 * of the snapshot's elements.
 */
export function attentionAt(found: SeenAt, seen: Seen, named: ReadonlyMap<number, string> | null, moments: readonly Moment[], at: number): Attention | null {
  if (named === null || found.moment.person) return null;
  const session = found.moment.call.session;
  const since = found.sinceMs;
  // The agent moves on at its next call after the snapshot.
  const next = moments.find((moment) => moment !== found.moment && !moment.person && moment.call.session === session && moment.startMs >= since);
  const presence = next === undefined || at < next.startMs ? 1 : Math.max(0, 1 - (at - next.startMs) / FADE_MS);
  const all = glances(seen, named, since);
  if (all.length === 0) return null;
  // A call cuts reading short: what wasn't read by then stays unread.
  const until = next === undefined ? at : Math.min(at, next.startMs);
  const lit = all
    .filter((glance) => glance.at <= until)
    .map((glance) => ({ glance, glow: Math.max(FLOOR, 1 - ((at - glance.at) / AFTERGLOW_MS) * (1 - FLOOR)) }));
  const last = lit.at(-1)?.glance ?? null;
  const done = lit.length === all.length || until < at;
  const head = done ? null : last;
  return { session, lit: presence === 0 ? [] : lit, head, gaze: last === null ? null : centre(last.frame), presence, since };
}

/**
 * Where an agent's pointer shows: on what it reads, unless it placed its
 * pointer by acting since the snapshot.
 */
export function pointerOf(agent: AgentScene, attention: Attention | null): DisplayPoint | null {
  if (attention === null || attention.session !== agent.session || attention.gaze === null) return agent.cursor;
  return agent.aimed !== null && agent.aimed >= attention.since ? agent.cursor : attention.gaze;
}

// The daemon's recordings, parsed once into what replay needs: each
// video with where it sits in the session, the trace's calls, and the
// arithmetic between a video's playhead and the session's clock.

import { type Callout, type Gesture, type Target, parseStarted } from "./overlay";
import type { Size } from "./geometry";
import { type TextRead, readText } from "./reading";
import { type Refs, readRefs } from "./tree";
import {
  type Recording as WireRecording,
  type RecordingList,
  TraceActor,
  TraceOutcome,
} from "../gen/doppel/daemon/v1/daemon_pb";

declare const brand: unique symbol;

/** A recording's name, as the daemon lists it (`2026-11-14T06-15-42Z-s1`). */
export type RecordingId = string & { readonly [brand]: "RecordingId" };

export interface RecordingSummary {
  id: RecordingId;
  started: Date;
  session: bigint;
  machines: string[];
  videos: number;
  calls: number;
  /** When its last call ended, in ms from the session's start. */
  tracedMs: number;
}

export interface Video {
  /** Its file name in the recording's folder. */
  file: string;
  machine: string;
  /** When in the session it starts, in ms; `null` when that wasn't recorded. */
  startMs: number | null;
  /** Its display's size in points, the space targets are in; `null` when that wasn't recorded. */
  points: Size | null;
}

export type Outcome = "done" | "failed" | "timed_out" | "refused" | { unknown: number };

export interface Call {
  kind: "call";
  startMs: number;
  endMs: number;
  machine: string | null;
  call: string;
  outcome: Outcome;
  error: string | null;
  /** How long it waited for a person or a resume first. */
  waitedMs: number | null;
  /** How long it then waited in its machine's line, behind other calls. */
  queuedMs: number | null;
  by: "session" | "person" | { unknown: number };
  /** What it did on the screen and where; `null` when the trace didn't keep it. */
  acted: { gesture: Gesture; target: Target; callout: Callout | null } | null;
  /** Its line in the trace, when the trace kept the snapshot it saw: OpenSeenSnapshot reads it. */
  seen: number | null;
  /** What the agent named that snapshot's elements. */
  refs: Refs | null;
  /** What it read off the screen by text recognition. */
  read: TextRead | null;
}

export type TraceLine = Call | { kind: "unreadable"; raw: string };

export interface Recording {
  id: RecordingId;
  videos: Video[];
  /**
   * In the order the calls started: the trace file has them as they
   * finished. Lines it can't read come last, in the file's order.
   */
  trace: TraceLine[];
}

/** Long enough to read a call that took no time, as the captions show it. */
export const MIN_SHOWN_MS = 1000;

export function parseRecordingList(list: RecordingList): RecordingSummary[] {
  return list.recordings.map((entry) => ({
    id: entry.id as RecordingId,
    started: new Date(Number(entry.startedUnixSeconds) * 1000),
    session: entry.session,
    machines: [...entry.machines],
    videos: entry.videos,
    calls: entry.calls,
    tracedMs: Number(entry.tracedMs),
  }));
}

function outcome(raw: TraceOutcome): Outcome {
  switch (raw) {
    case TraceOutcome.DONE:
      return "done";
    case TraceOutcome.FAILED:
      return "failed";
    case TraceOutcome.TIMED_OUT:
      return "timed_out";
    case TraceOutcome.REFUSED:
      return "refused";
    case TraceOutcome.UNSPECIFIED:
      return { unknown: raw };
    default:
      // One a newer daemon added.
      return { unknown: raw };
  }
}

function actor(raw: TraceActor): Call["by"] {
  switch (raw) {
    case TraceActor.SESSION:
      return "session";
    case TraceActor.PERSON:
      return "person";
    case TraceActor.UNSPECIFIED:
      return { unknown: raw };
    default:
      return { unknown: raw };
  }
}

export function parseRecording(recording: WireRecording): Recording {
  return {
    id: recording.id as RecordingId,
    videos: recording.videos.map((video) => ({
      file: video.file,
      machine: video.machine,
      startMs: video.startMs === undefined ? null : Number(video.startMs),
      points:
        video.displayPoints !== undefined && video.displayPoints.width > 0 && video.displayPoints.height > 0
          ? { width: video.displayPoints.width, height: video.displayPoints.height }
          : null,
    })),
    trace: inStartOrder(recording.trace.map((line, index): TraceLine => {
      switch (line.line.case) {
        case "call": {
          const call = line.line.value;
          return {
            kind: "call",
            startMs: Number(call.startMs),
            endMs: Number(call.endMs),
            machine: call.machine ?? null,
            call: call.call,
            outcome: outcome(call.outcome),
            error: call.error ?? null,
            waitedMs: call.waitedMs === undefined ? null : Number(call.waitedMs),
            queuedMs: call.queuedMs === undefined ? null : Number(call.queuedMs),
            by: actor(call.by),
            acted: call.acted === undefined ? null : parseStarted(call.acted),
            seen: call.seen ? index : null,
            refs: call.refs === undefined ? null : readRefs(call.refs),
            read: call.read === undefined ? null : readText(call.read),
          };
        }
        case "unreadable":
          return { kind: "unreadable", raw: line.line.value };
        case undefined:
          return { kind: "unreadable", raw: "" };
        default: {
          const never: never = line.line;
          return never;
        }
      }
    })),
  };
}

/** Calls by when they started (a stable sort: ties keep the file's order), then what can't be read. */
function inStartOrder(trace: TraceLine[]): TraceLine[] {
  const start = (line: TraceLine) => (line.kind === "call" ? line.startMs : Infinity);
  return [...trace].sort((a, b) => start(a) - start(b));
}

/** Where a recording's video is served: the gateway checks the token cookie. */
export function videoUrl(id: RecordingId, video: Video): string {
  return `/recordings/${encodeURIComponent(id)}/${encodeURIComponent(video.file)}`;
}

/** The session's time at `seconds` into `video`, or `null` if it can't be lined up. */
export function sessionMs(video: Video, seconds: number): number | null {
  return video.startMs === null ? null : video.startMs + seconds * 1000;
}

/** The trace's lines under the session's time `at`: calls on the video's machine, or on none. */
export function callsAt(trace: TraceLine[], machine: string, at: number): number[] {
  const under: number[] = [];
  trace.forEach((line, index) => {
    if (line.kind !== "call") return;
    if (line.machine !== null && line.machine !== machine) return;
    const end = Math.max(line.endMs, line.startMs + MIN_SHOWN_MS);
    if (line.startMs <= at && at < end) under.push(index);
  });
  return under;
}

/** Where to seek to see `call`: a video, and seconds into it. */
export interface Seek {
  video: number;
  seconds: number;
}

/**
 * The latest video that started by `call`'s start, on its machine (any
 * machine for a call that named none), and how far into it the call is;
 * `null` when no lined-up video has it.
 */
export function seekTo(videos: Video[], call: Call): Seek | null {
  return seekAt(videos, call.machine, call.startMs);
}

/** Where to seek to see the session's time `at` on `machine` (`null`: any machine). */
export function seekAt(videos: Video[], machine: string | null, at: number): Seek | null {
  let best: Seek | null = null;
  let bestStart = -Infinity;
  videos.forEach((video, index) => {
    if (video.startMs === null || video.startMs > at) return;
    if (machine !== null && video.machine !== machine) return;
    if (video.startMs >= bestStart) {
      bestStart = video.startMs;
      best = { video: index, seconds: (at - video.startMs) / 1000 };
    }
  });
  return best;
}

/** Whether any of a recording's videos can be lined up with its trace. */
export function aligned(recording: Recording): boolean {
  return recording.videos.some((video) => video.startMs !== null);
}

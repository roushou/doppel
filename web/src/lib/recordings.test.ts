import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { Gesture, RecordingSchema, TraceActor, TraceOutcome } from "../gen/doppel/daemon/v1/daemon_pb";
import { type Call, callsAt, parseRecording, type RecordingId, seekTo, sessionMs, type Video, videoUrl } from "./recordings";

const call = (startMs: number, endMs: number, machine: string | null, text = "x"): Call => ({
  kind: "call",
  startMs,
  endMs,
  machine,
  call: text,
  outcome: "done",
  acted: null,
  error: null,
  waitedMs: null,
  queuedMs: null,
  by: "session",
  seen: null,
  refs: null,
  read: null,
});

describe("recordings", () => {
  it("reads videos, calls, and a line or a value it doesn't know", () => {
    const recording = parseRecording(
      create(RecordingSchema, {
        id: "2026-11-14T06-15-42Z-s1",
        videos: [
          { file: "dev-001.mp4", machine: "dev", startMs: 98n },
          { file: "dev-002.mp4", machine: "dev" },
        ],
        trace: [
          {
            line: {
              case: "call",
              value: { startMs: 2000n, endMs: 7800n, machine: "dev", call: "a person had control", outcome: TraceOutcome.DONE, by: TraceActor.PERSON },
            },
          },
          { line: { case: "call", value: { startMs: 1n, endMs: 2n, call: "wait_for", outcome: 9 as TraceOutcome, error: "no VMM", waitedMs: 30n, queuedMs: 40n } } },
          { line: { case: "unreadable", value: "{\"start_ms\":1" } },
        ],
      }),
    );
    expect(recording.videos).toEqual([
      { file: "dev-001.mp4", machine: "dev", startMs: 98, points: null },
      { file: "dev-002.mp4", machine: "dev", startMs: null, points: null },
    ]);
    // By start: the 1 ms call, then the person's control.
    expect(recording.trace[0]).toMatchObject({ machine: null, outcome: { unknown: 9 }, error: "no VMM", waitedMs: 30, queuedMs: 40, by: { unknown: 0 } });
    expect(recording.trace[1]).toMatchObject({ kind: "call", by: "person", machine: "dev", startMs: 2000 });
    expect(recording.trace[2]).toEqual({ kind: "unreadable", raw: "{\"start_ms\":1" });
  });

  it("reads a video's display size and what a call did on its screen", () => {
    const recording = parseRecording(
      create(RecordingSchema, {
        videos: [{ file: "live-001.mp4", machine: "live", startMs: 0n, displayPoints: { width: 1280, height: 800 } }],
        trace: [
          {
            line: {
              case: "call",
              value: {
                startMs: 5n,
                endMs: 9n,
                machine: "live",
                call: "click",
                outcome: TraceOutcome.DONE,
                acted: { gesture: Gesture.CLICK, target: { case: "point", value: { x: 640, y: 400 } } },
              },
            },
          },
        ],
      }),
    );
    expect(recording.videos[0]?.points).toEqual({ width: 1280, height: 800 });
    expect(recording.trace[0]).toMatchObject({ acted: { gesture: "click", target: { kind: "point", at: { x: 640, y: 400 } } } });
  });

  it("lists calls by when they started, not when they finished", () => {
    const line = (startMs: bigint, call: string) => ({
      line: { case: "call" as const, value: { startMs, endMs: startMs + 1n, call, outcome: TraceOutcome.DONE } },
    });
    const recording = parseRecording(
      create(RecordingSchema, {
        trace: [line(110n, "status"), { line: { case: "unreadable", value: "?" } }, line(4017n, "type"), line(2013n, "wait"), line(4017n, "key")],
      }),
    );
    expect(recording.trace.map((l) => (l.kind === "call" ? l.call : l.raw))).toEqual(["status", "wait", "type", "key", "?"]);
  });

  it("points a call's kept snapshot at its line in the file, whatever order the calls are listed in", () => {
    const line = (startMs: bigint, call: string, seen: boolean) => ({
      line: { case: "call" as const, value: { startMs, endMs: startMs + 1n, call, outcome: TraceOutcome.DONE, seen } },
    });
    const recording = parseRecording(
      create(RecordingSchema, { trace: [line(500n, "snapshot", true), line(100n, "click", false), line(900n, "wait_for", true)] }),
    );
    expect(recording.trace.map((l) => (l.kind === "call" ? [l.call, l.seen] : null))).toEqual([
      ["click", null],
      ["snapshot", 0],
      ["wait_for", 2],
    ]);
  });

  it("puts a video's playhead on the session's clock, when it can", () => {
    expect(sessionMs({ file: "a", machine: "dev", startMs: 98, points: null }, 2.5)).toBe(2598);
    expect(sessionMs({ file: "a", machine: "dev", startMs: null, points: null }, 2.5)).toBeNull();
  });

  it("finds the calls under the playhead on the video's machine", () => {
    const trace = [call(100, 120, "dev"), call(500, 9000, "dev"), call(600, 700, "web"), call(800, 900, null), { kind: "unreadable" as const, raw: "" }];
    // A quick call stays under the playhead for a second, as its caption shows.
    expect(callsAt(trace, "dev", 110)).toEqual([0]);
    expect(callsAt(trace, "dev", 850)).toEqual([0, 1, 3]);
    expect(callsAt(trace, "dev", 1100)).toEqual([1, 3]);
    expect(callsAt(trace, "dev", 1800)).toEqual([1]);
    expect(callsAt(trace, "web", 650)).toEqual([2]);
    expect(callsAt(trace, "dev", 9500)).toEqual([]);
  });

  it("seeks to a call in the latest video of its machine that had started", () => {
    const videos: Video[] = [
      { file: "dev-001.mp4", machine: "dev", startMs: 100, points: null },
      { file: "web-001.mp4", machine: "web", startMs: 200, points: null },
      { file: "dev-002.mp4", machine: "dev", startMs: 5000, points: null },
      { file: "dev-003.mp4", machine: "dev", startMs: null, points: null },
    ];
    expect(seekTo(videos, call(1100, 1200, "dev"))).toEqual({ video: 0, seconds: 1 });
    expect(seekTo(videos, call(6000, 6100, "dev"))).toEqual({ video: 2, seconds: 1 });
    expect(seekTo(videos, call(1200, 1300, "web"))).toEqual({ video: 1, seconds: 1 });
    // A call that named no machine: whichever video was showing.
    expect(seekTo(videos, call(300, 400, null))).toEqual({ video: 1, seconds: 0.1 });
    // Before any video, or on a machine with none lined up.
    expect(seekTo(videos, call(50, 60, "dev"))).toBeNull();
    expect(seekTo(videos, call(1000, 1100, "mail"))).toBeNull();
  });

  it("names a video's address by its recording and file", () => {
    expect(videoUrl("2026-11-14T06-15-42Z-s1" as RecordingId, { file: "live-001.mp4", machine: "live", startMs: 0, points: null })).toBe(
      "/recordings/2026-11-14T06-15-42Z-s1/live-001.mp4",
    );
  });
});

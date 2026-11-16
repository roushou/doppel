import { describe, expect, it } from "vitest";
import { Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, started } from "./activity.fixture";
import { type Moment, record, reading, readAt, type TextLook } from "./timeline";

const done = { case: "done", value: {} };
const rect = (x: number, y: number, width: number, height: number) => ({ origin: { x, y }, size: { width, height } });
const read = (text: string) => ({
  text: {
    displayPoints: { width: 1280, height: 800 },
    lines: [
      { text, frame: rect(10, 40, 120, 20), confidence: 1 },
      { text: "no frame", confidence: 1 },
    ],
  },
  query: "can",
  found: [rect(10, 40, 30, 20)],
});

function find(moments: readonly Moment[], request: number, at: number, text: string): readonly Moment[] {
  const look = started(request, Gesture.LOOK, { case: undefined });
  return record(record(moments, look, at), ended(request, done, undefined, read(text)), at + 50);
}

describe("text the agent read", () => {
  it("comes with the call's end: its lines, what it looked for, and where that was found", () => {
    const shown = readAt(find([], 1, 0, "Cancel"), 100);
    expect(shown?.read).toEqual({
      lines: [{ text: "Cancel", frame: { left: 10, top: 40, width: 120, height: 20 } }],
      query: "can",
      found: [{ left: 10, top: 40, width: 30, height: 20 }],
    });
    expect(shown?.sinceMs).toBe(50);
  });

  it("is the newer of the agent's read and a person's own, and dims once the screen may have changed", () => {
    let moments = find([], 1, 0, "Save");
    const mine: TextLook = { atMs: 200, outcome: { kind: "loading" } };
    expect(reading(moments, null, 100).kind).toBe("agent");
    expect(reading(moments, mine, 300)).toMatchObject({ kind: "person", actedSince: false });
    moments = record(moments, started(2, Gesture.KEY, { case: undefined }), 350);
    expect(reading(moments, mine, 360)).toMatchObject({ kind: "person", actedSince: true });
    moments = find(moments, 3, 400, "Open");
    const latest = reading(moments, mine, 500);
    expect(latest.kind === "agent" && latest.found.read.lines[0]?.text).toBe("Open");
    expect(reading([], null, 0).kind).toBe("nothing");
  });
});

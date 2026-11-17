import { describe as group, expect, it } from "vitest";
import { Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, started } from "./activity.fixture";
import { SWEEP_MS, describe, sweepEnds } from "./overlay";

group("reading the daemon's activity", () => {
  it("reads an action on an element as its frame and name", () => {
    const pressed = started(1, Gesture.ACT, {
      case: "element",
      value: { frame: { origin: { x: 100, y: 200 }, size: { width: 40, height: 20 } }, label: "Equals" },
    });
    expect(pressed).toMatchObject({
      kind: "started",
      call: { key: "1:1", gesture: "act", target: { kind: "element", frame: { left: 100, top: 200, width: 40, height: 20 } } },
    });
    expect(pressed.kind === "started" && describe(pressed.call)).toBe("“Equals”");
  });

  it("reads a timeout as a failure", () => {
    const event = ended(1, { case: "timedOut", value: "" });
    expect(event.kind === "ended" && event.ending).toEqual({ kind: "failed", reason: "timed out" });
  });

  it("drops a target with an empty frame or a missing point", () => {
    const empty = started(1, Gesture.ACT, {
      case: "element",
      value: { frame: { origin: { x: 1, y: 1 }, size: { width: 0, height: 5 } }, label: "x" },
    });
    expect(empty.kind === "started" && empty.call.target).toEqual({ kind: "none" });
    const half = started(2, Gesture.DRAG, { case: "drag", value: { from: { x: 1, y: 1 } } });
    expect(half.kind === "started" && half.call.target).toEqual({ kind: "none" });
  });

  it("reads a gesture a newer daemon added as another call", () => {
    const event = started(1, 99 as Gesture, { case: undefined });
    expect(event.kind === "started" && event.call.gesture).toBe("other");
  });
});

group("the reading band", () => {
  it("leaves the screen at the end of a pass, never midway", () => {
    expect(sweepEnds(100, 150)).toBe(100 + SWEEP_MS);
    expect(sweepEnds(0, SWEEP_MS)).toBe(SWEEP_MS);
    expect(sweepEnds(0, SWEEP_MS + 1)).toBe(2 * SWEEP_MS);
    expect(sweepEnds(0, null)).toBe(Infinity);
  });
});

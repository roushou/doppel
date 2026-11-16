import { describe, expect, it } from "vitest";
import { Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, marked, started } from "./activity.fixture";
import { MARK_MS, SWEEP_MS, arrowEnds, describe as describeCall } from "./overlay";
import type { Call as TraceCall } from "./recordings";
import { KEPT, MIN_UNDER_WAY_MS, type Moment, family, fromTrace, lanes, record, sceneAt, within } from "./timeline";
import type { AgentScene, Scene } from "./overlay";

/** The one session's part of a scene (the fixtures' session 1, or a recording's 0). */
function agent(scene: Scene): AgentScene {
  const only = scene.agents[0];
  if (only === undefined || scene.agents.length !== 1) throw new Error(`${scene.agents.length} agents`);
  return only;
}

const click = (request: number, x: number, y: number) => started(request, Gesture.CLICK, { case: "point", value: { x, y } });

function live(...events: [ReturnType<typeof started>, number][]): readonly Moment[] {
  return events.reduce<readonly Moment[]>((moments, [event, at]) => record(moments, event, at), []);
}

describe("a machine's timeline", () => {
  it("puts the pointer where a click lands and leaves a ripple there", () => {
    const moments = live([click(1, 10, 20), 0]);
    const scene = sceneAt(moments, 100);
    expect(agent(scene).cursor).toEqual({ x: 10, y: 20 });
    expect(agent(scene).current?.call.gesture).toBe("click");
    expect(agent(scene).marks).toMatchObject([{ kind: "ripple", at: { x: 10, y: 20 }, since: 0 }]);
    // The mark fades; the pointer stays.
    expect(agent(sceneAt(moments, MARK_MS)).marks).toEqual([]);
    expect(agent(sceneAt(moments, MARK_MS)).cursor).toEqual({ x: 10, y: 20 });
  });

  it("shows nothing before a call starts", () => {
    const moments = live([click(1, 10, 20), 500]);
    expect(sceneAt(moments, 400).agents).toEqual([]);
  });

  it("keeps the pointer where it was for a call with no place", () => {
    const moments = live([click(1, 5, 5), 0], [started(2, Gesture.TYPE, { case: undefined }), 10]);
    const scene = sceneAt(moments, 20);
    expect(agent(scene).cursor).toEqual({ x: 5, y: 5 });
    expect(agent(scene).current?.call.gesture).toBe("type");
  });

  it("ends a call by its key, and marks a failure where the pointer is", () => {
    const moments = live([click(1, 5, 5), 0], [click(2, 8, 8), 10], [ended(1, { case: "done", value: {} }), 20]);
    expect(moments.map((m) => m.endMs)).toEqual([20, null]);
    const failed = record(moments, ended(2, { case: "failed", value: "no agent" }), 2000);
    expect(failed[1]).toMatchObject({ endMs: 2000, failed: "no agent" });
    const scene = sceneAt(failed, 2100);
    expect(agent(scene).current).toBeNull();
    expect(agent(scene).marks).toMatchObject([{ kind: "failed", at: { x: 8, y: 8 }, reason: "no agent" }]);
  });

  it("shows a quick call under way long enough to read", () => {
    const moments = live([click(1, 1, 1), 0], [ended(1, { case: "done", value: {} }), 5]);
    expect(agent(sceneAt(moments, MIN_UNDER_WAY_MS - 1)).current).not.toBeNull();
    expect(agent(sceneAt(moments, MIN_UNDER_WAY_MS)).current).toBeNull();
  });

  it("finishes the reading band's pass after a quick look ends", () => {
    const look = started(1, Gesture.LOOK, { case: undefined });
    const quick = live([look, 0], [ended(1, { case: "done", value: {} }), 300]);
    expect(agent(sceneAt(quick, 400)).scan).toBe(0);
    expect(agent(sceneAt(quick, SWEEP_MS - 1)).scan).toBe(0);
    expect(agent(sceneAt(quick, SWEEP_MS)).scan).toBeNull();
    // A longer look runs to the end of the pass it ends in.
    const long = live([look, 0], [ended(1, { case: "done", value: {} }), SWEEP_MS + 100]);
    expect(agent(sceneAt(long, 2 * SWEEP_MS - 1)).scan).toBe(0);
    expect(agent(sceneAt(long, 2 * SWEEP_MS)).scan).toBeNull();
    // One still looking keeps sweeping.
    expect(agent(sceneAt(live([look, 0]), 10 * SWEEP_MS)).scan).toBe(0);
  });

  it("keeps only the latest moments", () => {
    const events: [ReturnType<typeof started>, number][] = [];
    for (let i = 0; i < KEPT + 10; i++) events.push([click(i, 0, 0), i]);
    const moments = live(...events);
    expect(moments).toHaveLength(KEPT);
    expect(moments[0]?.key).toBe("1:10");
  });

  it("replays a trace's calls on a machine, a person's control drawing nothing", () => {
    const traced = (startMs: number, machine: string | null, by: TraceCall["by"], acted: TraceCall["acted"]): TraceCall => ({
      kind: "call",
      startMs,
      endMs: startMs + 100,
      machine,
      call: "c",
      outcome: "done",
      error: null,
      waitedMs: null,
      queuedMs: null,
      by,
      acted,
      seen: null,
      refs: null,
      read: null,
    });
    const at = (x: number) => ({ gesture: "click" as const, target: { kind: "point" as const, at: { x, y: 0 } }, callout: null });
    const moments = fromTrace(
      [traced(0, "dev", "session", at(1)), traced(50, "web", "session", at(2)), traced(100, "dev", "person", at(3)), traced(200, null, "session", null)],
      "dev",
    );
    expect(moments.map((m) => m.startMs)).toEqual([0, 100, 200]);
    expect(moments[2]?.call.gesture).toBe("other");
    expect(agent(sceneAt(moments, 150)).cursor).toEqual({ x: 1, y: 0 });
  });

  it("finds the moments a window shows, an unfinished one up to now", () => {
    const moments = live([click(1, 0, 0), 0], [ended(1, { case: "done", value: {} }), 10], [click(2, 0, 0), 50]);
    expect(within(moments, 20, 40, 60).map((m) => m.key)).toEqual([]);
    expect(within(moments, 20, 55, 60).map((m) => m.key)).toEqual(["1:2"]);
    expect(within(moments, 5, 55, 60).map((m) => m.key)).toEqual(["1:1", "1:2"]);
  });

  it("colours gestures by what they touch", () => {
    expect([family("act"), family("type"), family("look"), family("lifecycle")]).toEqual(["pointer", "keys", "look", "machine"]);
  });
});

describe("an agent's marks", () => {
  it("show for their time on their own place, without moving the pointer", () => {
    const moments = live([click(1, 1, 2), 0], [marked(2, { case: "forMs", value: 5000 }), 100], [ended(2, { case: "done", value: {} }), 101]);
    const scene = sceneAt(moments, 2000);
    expect(agent(scene).cursor).toEqual({ x: 1, y: 2 });
    expect(scene.callouts).toMatchObject([
      { since: 100, until: 5100, callout: { on: { kind: "point", at: { x: 40, y: 50 } }, shape: { kind: "dot" }, note: "here" } },
    ]);
    expect(sceneAt(moments, 5100).callouts).toEqual([]);
    expect(describeCall(moments[1]!.call)).toBe("Pointing out: here");
    expect(family("mark")).toBe("mark");
  });

  it("until acted, go when the agent next changes the screen, not when it looks", () => {
    const moments = live(
      [marked(1, { case: "untilActedMs", value: 5000 }), 0],
      [started(2, Gesture.LOOK, { case: undefined }), 300],
      [click(3, 40, 50), 900],
    );
    expect(sceneAt(moments, 800).callouts).toHaveLength(1);
    expect(sceneAt(moments, 900).callouts).toEqual([]);
  });

  it("show nothing when the mark call failed", () => {
    const moments = live([marked(1, { case: "forMs", value: 5000 }), 0], [ended(1, { case: "refused", value: "no" }), 1]);
    expect(sceneAt(moments, 500).callouts).toEqual([]);
  });

  it("point arrows from where there is room to just outside what they mark", () => {
    const picture = { left: 0, top: 0, width: 1000, height: 800 };
    const { tail, tip } = arrowEnds({ left: 300, top: 300, width: 100, height: 40 }, null, picture);
    expect(tail).toEqual({ x: 190, y: 220 });
    expect(tip.y).toBeCloseTo(296);
    expect(arrowEnds({ left: 10, top: 10, width: 40, height: 20 }, null, picture).tail).toEqual({ x: 160, y: 110 });
    expect(arrowEnds({ left: 500, top: 500, width: 0, height: 0 }, { x: 500, y: 300 }, picture)).toEqual({
      tail: { x: 500, y: 300 },
      tip: { x: 500, y: 496 },
    });
  });
});

describe("several agents on one machine", () => {
  it("each have their own pointer, call and lane", () => {
    const by = (session: number, request: number, x: number) => {
      const event = click(request, x, x);
      if (event.kind !== "started") throw new Error("a start");
      return { ...event, call: { ...event.call, key: `${session}:${request}`, session, client: `agent ${session}` } };
    };
    const moments = live([by(4, 1, 1), 0], [by(2, 1, 9), 10]);
    const scene = sceneAt(moments, 30);
    expect(scene.agents.map((a) => [a.session, a.client, a.cursor])).toEqual([
      [2, "agent 2", { x: 9, y: 9 }],
      [4, "agent 4", { x: 1, y: 1 }],
    ]);
    expect(lanes(moments).map((lane) => [lane.session, lane.client, lane.moments.length])).toEqual([
      [2, "agent 2", 1],
      [4, "agent 4", 1],
    ]);
  });
});

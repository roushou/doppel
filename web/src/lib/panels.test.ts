import { describe, expect, it } from "vitest";
import { Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, started } from "./activity.fixture";
import type { MachineName } from "./machines";
import { type Panels, begin, disconnected, finish, handedBack, held, keepReportsOf, panelOf, recorded, reported, took } from "./panels";
import type { Report } from "./report";

const name = (raw: string) => raw as MachineName;
const none: Panels = new Map();
const report: Report = { guest: { kind: "not-running" }, checkpoints: 2 };

describe("the panels", () => {
  it("run one action at a time on a machine", () => {
    const busy = begin(none, name("a"), "stop");
    expect(busy).not.toBeNull();
    if (busy === null) return;
    expect(panelOf(busy, name("a")).action).toBe("stop");
    expect(begin(busy, name("a"), "start")).toBeNull();
    // Another machine isn't held up.
    expect(begin(busy, name("b"), "start")).not.toBeNull();
    expect(begin(finish(busy, name("a")), name("a"), "start")).not.toBeNull();
  });

  it("know which machines this page controls", () => {
    const panels = took(took(none, name("a")), name("b"));
    expect(held(panels)).toEqual([name("a"), name("b")]);
    expect(held(handedBack(panels, name("a")))).toEqual([name("b")]);
  });

  it("record an agent's calls on its machine's timeline", () => {
    const panels = recorded(recorded(none, started(1, Gesture.LOOK, { case: undefined }), 10), ended(1, { case: "done", value: {} }), 30);
    const [moment] = panelOf(panels, name("dev")).timeline;
    expect(moment).toMatchObject({ startMs: 10, endMs: 30 });
    expect(panelOf(panels, name("other")).timeline).toEqual([]);
  });

  it("keep reports for the machines on the stage only", () => {
    const staged = [name("a"), name("b")];
    expect(panelOf(reported(none, name("a"), report, [name("b")]), name("a")).report).toBeNull();
    const panels = reported(reported(none, name("a"), report, staged), name("b"), report, staged);
    expect(panelOf(panels, name("a")).report).toEqual(report);
    // A failed read drops the one kept.
    expect(panelOf(reported(panels, name("a"), null, staged), name("a")).report).toBeNull();
    const kept = keepReportsOf(panels, [name("b")]);
    expect(panelOf(kept, name("a")).report).toBeNull();
    expect(panelOf(kept, name("b")).report).toEqual(report);
  });

  it("lose control and actions with the connection, but keep timelines", () => {
    const busy = begin(took(recorded(none, started(1, Gesture.LOOK, { case: undefined }), 0), name("dev")), name("dev"), "pause");
    if (busy === null) throw new Error("busy");
    const after = panelOf(disconnected(busy), name("dev"));
    expect(after).toMatchObject({ action: null, control: "free" });
    expect(after.timeline).toHaveLength(1);
  });

  it("leave the panels they were given as they were", () => {
    const panels = took(none, name("a"));
    handedBack(panels, name("a"));
    expect(held(panels)).toEqual([name("a")]);
  });
});

import { describe, expect, it } from "vitest";
import { type Machine, type MachineName, UNRECORDED } from "./machines";
import { actions, checkpointLabel, machineName } from "./lifecycle";
import {
  type Bay,
  type Stage,
  bays,
  closeLauncher,
  escape,
  full,
  keep,
  keepBays,
  onStage,
  readBays,
  settle,
  showSlot,
  shows,
  slotOf,
  stageOf,
  startFrom,
  startIn,
  toggleLauncher,
  toggleSplit,
} from "./stage";

const stopped = (name: string): Machine => ({ name: name as MachineName, state: { kind: "stopped" }, lineage: UNRECORDED });
const running = (name: string): Machine => ({
  name: name as MachineName,
  state: { kind: "live", run: "running" },
  lineage: UNRECORDED,
});
const name = (raw: string) => raw as MachineName;

describe("the bays", () => {
  const kinds = (shown: Bay[]) => shown.map((bay) => (bay.kind === "machine" ? bay.machine.name : null));

  it("are two, live machines in list order, the rest empty", () => {
    expect(kinds(bays([stopped("dev"), running("live")], []))).toEqual(["live", null]);
    expect(kinds(bays([], []))).toEqual([null, null]);
    expect(kinds(bays([running("a"), running("b"), running("c")], []))).toEqual(["a", "b"]);
  });

  it("keep a live machine in its bay", () => {
    const machines = [running("a"), running("b")];
    expect(kinds(bays(machines, [name("b"), name("a")]))).toEqual(["b", "a"]);
    expect(kinds(bays([stopped("a"), running("b")], [name("a"), name("b")]))).toEqual([null, "b"]);
  });

  it("put a machine started from a bay in that bay", () => {
    const kept = startIn([null, null], 1, name("dev"));
    expect(kept).toEqual([null, "dev"]);
    expect(kinds(bays([running("dev")], kept))).toEqual([null, "dev"]);
    expect(keep(bays([stopped("dev")], kept), kept)).toEqual([null, "dev"]);
  });

  it("are full when both hold a machine", () => {
    expect(full(bays([running("a"), running("b")], []))).toBe(true);
    expect(full(bays([running("a"), stopped("b")], []))).toBe(false);
  });

  it("show the picked slot's machine alone, or both when split", () => {
    const names = (machines: Machine[]) => machines.map((machine) => machine.name);
    const two = bays([running("a"), running("b")], []);
    expect(names(onStage(two, 1, false))).toEqual(["b"]);
    expect(names(onStage(two, 0, true))).toEqual(["a", "b"]);
    const one = bays([running("a")], [null, null]);
    expect(names(onStage(one, 1, false))).toEqual(["a"]);
    expect(names(onStage(one, 1, true))).toEqual(["a"]);
    expect(onStage(bays([stopped("a")], []), 0, false)).toEqual([]);
  });

  it("are kept between loads, dropping what isn't a name", () => {
    expect(readBays(keepBays([name("live"), null]))).toEqual(["live", null]);
    expect(readBays("Not A Name\ndev\nextra")).toEqual([null, "dev"]);
    expect(readBays("")).toEqual([null, null]);
  });
});

describe("lifecycle", () => {
  it("offers what each state allows", () => {
    expect(actions({ kind: "stopped" })).toContain("start");
    expect(actions({ kind: "stopped" })).toContain("delete");
    expect(actions({ kind: "live", run: "running" })).not.toContain("delete");
    expect(actions({ kind: "live", run: "running" })).toContain("roll-back");
    expect(actions({ kind: "live", run: "paused" })).toContain("resume");
    expect(actions({ kind: "live", run: "starting" })).toEqual(["force-stop"]);
    expect(actions({ kind: "unreadable", reason: "?" })).toEqual(["delete"]);
  });

  it("checks names and labels as the daemon does", () => {
    expect(machineName(" live-3 ")).toBe("live-3");
    for (const wrong of ["", "-a", "Live", "a_b", "a".repeat(33)]) expect(machineName(wrong)).toBeNull();
    expect(checkpointLabel("before_upgrade-2")).toBe("before_upgrade-2");
    for (const wrong of ["", "Ready", "a.b", "a".repeat(65)]) expect(checkpointLabel(wrong)).toBeNull();
  });
});

describe("the stage", () => {
  const both = [running("a"), running("b")];
  const names = (stage: Stage, machines: Machine[]) => shows(stage, bays(machines, stage.kept)).map((machine) => machine.name);

  it("shows the picked slot alone unless split with both running", () => {
    let stage = stageOf([]);
    expect(names(stage, both)).toEqual(["a"]);
    stage = showSlot(stage, 1);
    expect(names(stage, both)).toEqual(["b"]);
    stage = toggleSplit(stage);
    expect(names(stage, both)).toEqual(["a", "b"]);
    expect(names(stage, [running("a")])).toEqual(["a"]);
  });

  it("leaves the split and closes the launcher when a slot is shown", () => {
    const stage = showSlot(toggleLauncher(toggleSplit(stageOf([])), 1), 0);
    expect(stage).toMatchObject({ picked: 0, split: "off", launching: null });
  });

  it("toggles a launcher, another slot's replacing it", () => {
    let stage = toggleLauncher(stageOf([]), 0);
    expect(stage.launching).toBe(0);
    stage = toggleLauncher(stage, 1);
    expect(stage.launching).toBe(1);
    expect(toggleLauncher(stage, 1).launching).toBeNull();
    expect(closeLauncher(stage).launching).toBeNull();
  });

  it("puts a machine started from a slot there, and shows it", () => {
    const stage = startFrom(toggleLauncher(stageOf([]), 1), 1, name("dev"));
    expect(stage.launching).toBeNull();
    const live = [running("other"), running("dev")];
    expect(slotOf(bays(live, stage.kept), name("dev"))).toBe(1);
    expect(names(stage, live)).toEqual(["dev"]);
    expect(slotOf(bays(live, stage.kept), name("gone"))).toBeNull();
  });

  it("settles a live machine in its bay, and is the same stage when nothing moved", () => {
    const first = stageOf([]);
    const settled = settle(first, bays([running("b")], first.kept));
    expect(settled.kept).toEqual(["b", null]);
    expect(settle(settled, bays([running("b")], settled.kept))).toBe(settled);
    expect(slotOf(bays(both, settled.kept), name("b"))).toBe(0);
  });

  it("closes the launcher on Escape first, then leaves the split unless Escape is meant elsewhere", () => {
    const split = toggleLauncher(toggleSplit(stageOf([])), 0);
    const once = escape(split, "stage");
    expect(once).toMatchObject({ launching: null, split: "on" });
    expect(escape(once, "elsewhere").split).toBe("on");
    expect(escape(once, "stage").split).toBe("off");
  });
});

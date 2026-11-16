import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { AxSnapshotSchema, Truncation } from "../gen/doppel/agent/v1/agent_pb";
import { ActivitySchema, Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, started } from "./activity.fixture";
import { AFTERGLOW_MS, FADE_MS, FLOOR, GLANCE_MAX_MS, MOST_GLANCES, attentionAt, glances, pointerOf } from "./attention";
import { parseActivity } from "./overlay";
import { type Moment, record, sceneAt, seenAt } from "./timeline";
import { type Seen, readSnapshot, refsOf } from "./tree";

const frame = (x: number, y: number, width: number, height: number) => ({ origin: { x, y }, size: { width, height } });

/** A page: a window, a group, a heading, a link, an unnamed image, a field, and a button off screen. */
const page = () =>
  create(AxSnapshotSchema, {
    snapshotId: 7n,
    app: { pid: 9, bundleId: "com.google.Chrome", name: "Google Chrome" },
    truncation: Truncation.COMPLETE,
    nodes: [
      { role: "AXWindow", title: "Shop", frame: frame(0, 0, 1000, 800) },
      { parent: 0, role: "AXGroup", title: "Main", frame: frame(0, 0, 1000, 800) },
      { parent: 1, role: "AXHeading", title: "Plans", frame: frame(40, 40, 300, 40) },
      { parent: 1, role: "AXLink", title: "Pricing", frame: frame(40, 100, 80, 20) },
      { parent: 1, role: "AXImage", frame: frame(400, 100, 200, 200) },
      { parent: 1, role: "AXTextField", title: "Email", frame: frame(40, 140, 240, 24) },
      { parent: 1, role: "AXButton", title: "Later", frame: frame(40, 2000, 80, 24) },
    ],
  });

const done = { case: "done", value: {} };

/** The page seen by a snapshot ending at 100, its elements named in `order`, then `after` events. */
function scene(order: number[], ...after: [ReturnType<typeof started>, number][]) {
  const named = parseActivity(
    create(ActivitySchema, { session: 1n, request: 1n, machine: "dev", phase: { case: "named", value: { snapshot: 7n, generation: 2, nodes: order } } }),
  );
  if (named === null) throw new Error("unread");
  const events: [ReturnType<typeof started>, number][] = [
    [started(1, Gesture.LOOK, { case: undefined }), 0],
    [ended(1, done, page()), 100],
    [named, 110],
    ...after,
  ];
  const moments = events.reduce<readonly Moment[]>((moments, [event, at]) => record(moments, event, at), []);
  const found = seenAt(moments, 200);
  if (found?.source.kind !== "live") throw new Error("expected a live snapshot");
  const seen: Seen = found.source.seen;
  return { moments, found, seen, named: refsOf(seen, found.moment.refs) };
}

describe("what an agent attends to", () => {
  it("reads the elements it was shown in their order, skipping frames, containers and what's off screen", () => {
    const { seen, named } = scene([0, 1, 5, 2, 3, 4, 6]);
    if (named === null) throw new Error("no refs");
    const read = glances(seen, named, 100);
    // The field was named before the heading: it's read first.
    expect(read.map((glance) => [glance.index, glance.ref])).toEqual([
      [5, "s2e3"],
      [2, "s2e4"],
      [3, "s2e5"],
      [4, "s2e6"],
    ]);
    expect(read[0]?.label).toBe("s2e3 Text Field “Email”");
    expect(read.map((glance) => glance.at)).toEqual([0, 1, 2, 3].map((step) => 100 + step * GLANCE_MAX_MS));
  });

  it("reads only so many elements, quicker the more there are", () => {
    const many = create(AxSnapshotSchema, {
      snapshotId: 1n,
      nodes: Array.from({ length: 200 }, (_, index) => ({ role: "AXButton", title: `b${index}`, frame: frame(index, 0, 5, 5) })),
    });
    const seen = readSnapshot(many);
    const read = glances(seen, new Map(seen.elements.map((element) => [element.index, `e${element.index}`])), 0);
    expect(read).toHaveLength(MOST_GLANCES);
    expect(read[1]?.at).toBeLessThan(GLANCE_MAX_MS);
  });

  it("lights each element as it's read, its glow settling to a floor, its eyes on the latest", () => {
    const { moments, found, seen, named } = scene([2, 3, 5]);
    const before = attentionAt(found, seen, named, moments, 100);
    expect(before?.lit.map((lit) => lit.glance.index)).toEqual([2]);
    expect(before?.head?.index).toBe(2);
    expect(before?.gaze).toEqual({ x: 190, y: 60 });

    const reading = attentionAt(found, seen, named, moments, 100 + GLANCE_MAX_MS);
    expect(reading?.lit.map((lit) => lit.glance.index)).toEqual([2, 3]);
    expect(reading?.head?.index).toBe(3);
    expect(reading?.lit[1]?.glow).toBe(1);
    expect(reading?.lit[0]?.glow).toBeLessThan(1);

    const after = attentionAt(found, seen, named, moments, 10_000);
    expect(after?.head).toBeNull();
    expect(after?.lit.every((lit) => lit.glow === FLOOR)).toBe(true);
    // Its eyes stay on the last it read.
    expect(after?.gaze).toEqual({ x: 160, y: 152 });
    expect(after?.presence).toBe(1);
    expect(AFTERGLOW_MS).toBeLessThan(10_000);
  });

  it("stops reading when the agent calls again, and fades", () => {
    const click = started(2, Gesture.CLICK, { case: "point", value: { x: 1, y: 1 } });
    const { moments, found, seen, named } = scene([2, 3, 5], [click, 150]);
    const cut = attentionAt(found, seen, named, moments, 150 + FADE_MS / 2);
    // Only what was read by the click stays lit, and reading is over.
    expect(cut?.lit.map((lit) => lit.glance.index)).toEqual([2]);
    expect(cut?.head).toBeNull();
    expect(cut?.presence).toBeCloseTo(0.5);
    // Faded, nothing is lit, but its eyes stay where they were.
    const faded = attentionAt(found, seen, named, moments, 150 + FADE_MS);
    expect(faded?.lit).toEqual([]);
    expect(faded?.gaze).toEqual({ x: 190, y: 60 });
  });

  it("is nothing when the agent named none of the snapshot", () => {
    const { moments, found, seen } = scene([]);
    expect(attentionAt(found, seen, null, moments, 200)).toBeNull();
    expect(attentionAt(found, seen, new Map(), moments, 200)).toBeNull();
  });

  it("puts the pointer on what it reads, until it acts somewhere", () => {
    const click = started(2, Gesture.CLICK, { case: "point", value: { x: 1, y: 2 } });
    const { moments, found, seen, named } = scene([2, 3], [click, 5000]);
    const at = (ms: number) => {
      const agent = sceneAt(moments, ms).agents[0];
      if (agent === undefined) throw new Error("no agent");
      return pointerOf(agent, attentionAt(found, seen, named, moments, ms));
    };
    expect(at(100)).toEqual({ x: 190, y: 60 });
    expect(at(4000)).toEqual({ x: 80, y: 110 });
    expect(at(5000)).toEqual({ x: 1, y: 2 });
  });
});

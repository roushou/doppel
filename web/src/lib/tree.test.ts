import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { AxSnapshotSchema, Truncation } from "../gen/doppel/agent/v1/agent_pb";
import { Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { ended, started } from "./activity.fixture";
import { type Look, type Moment, record, seenAt, showing } from "./timeline";
import { ONE_LINE_MAX, ancestors, elementAt, initiallyFolded, kindOf, oneLine, readRefs, readSnapshot, refsOf, reveal, rows } from "./tree";

const frame = (x: number, y: number, width: number, height: number) => ({ origin: { x, y }, size: { width, height } });

/** A window holding an unnamed group, which holds a button and a field; and a menu bar beside it. */
const wire = () =>
  create(AxSnapshotSchema, {
    snapshotId: 3n,
    app: { pid: 412, bundleId: "com.apple.calculator", name: "Calculator" },
    truncation: Truncation.COMPLETE,
    nodes: [
      { role: "AXApplication", title: "Calculator" },
      { parent: 0, role: "AXWindow", title: "Calculator", frame: frame(100, 80, 400, 300) },
      { parent: 1, role: "AXGroup", frame: frame(110, 120, 380, 240) },
      { parent: 2, role: "AXButton", description: "Equals", frame: frame(400, 300, 48, 48), flags: { enabled: true }, actions: ["AXPress"] },
      { parent: 2, role: "AXTextField", title: "Amount", value: { value: { case: "text", value: "42" } }, frame: frame(120, 130, 200, 24) },
      { parent: 0, role: "AXMenuBar", frame: frame(0, 0, 1280, 24) },
      { parent: 9, role: "AXHologram", title: "From the future" },
    ],
  });

describe("an accessibility snapshot, read", () => {
  it("is a tree with each element's kind, name and value", () => {
    const seen = readSnapshot(wire());
    expect(seen.app?.name).toBe("Calculator");
    expect(seen.truncated).toBe(false);
    expect(seen.elements.map((e) => [e.index, e.parent, e.depth])).toEqual([
      [0, null, 0],
      [1, 0, 1],
      [2, 1, 2],
      [3, 2, 3],
      [4, 2, 3],
      [5, 0, 1],
      // Its parent comes after it: read as a root, not dropped.
      [6, null, 0],
    ]);
    expect(seen.elements[0]?.children).toEqual([1, 5]);
    const button = seen.elements[3];
    expect(button).toMatchObject({ kind: "control", name: "Equals", roleWords: "Button", states: { enabled: true }, actions: ["AXPress"] });
    expect(seen.elements[4]).toMatchObject({ kind: "field", name: "Amount", value: "42" });
    expect(seen.elements[2]).toMatchObject({ kind: "container", name: "", wrapper: true });
    // A role this page doesn't know is kept as it is.
    expect(seen.elements[6]).toMatchObject({ kind: "other", role: "AXHologram", roleWords: "Hologram" });
    expect(seen.elements[0]?.frame).toBeNull();
  });

  it("names an element by its text when nothing else names it, and says when the snapshot stopped early", () => {
    const text = create(AxSnapshotSchema, {
      truncation: Truncation.NODE_BUDGET,
      nodes: [{ role: "AXStaticText", value: { value: { case: "text", value: "Hello" } } }],
    });
    const seen = readSnapshot(text);
    expect(seen.elements[0]).toMatchObject({ kind: "text", name: "Hello", value: null });
    expect(seen.truncated).toBe(true);
  });

  it("lifts an unnamed wrapper's children into its place when wrappers are hidden", () => {
    const seen = readSnapshot(wire());
    const at = (wrappers: "show" | "hide") => rows(seen, wrappers).map(({ index, depth }) => ({ index, depth }));
    expect(at("show")).toEqual([
      { index: 0, depth: 0 },
      { index: 1, depth: 1 },
      { index: 2, depth: 2 },
      { index: 3, depth: 3 },
      { index: 4, depth: 3 },
      { index: 5, depth: 1 },
      { index: 6, depth: 0 },
    ]);
    expect(at("hide")).toEqual([
      { index: 0, depth: 0 },
      { index: 1, depth: 1 },
      { index: 3, depth: 2 },
      { index: 4, depth: 2 },
      { index: 5, depth: 1 },
      { index: 6, depth: 0 },
    ]);
  });

  it("draws each row's branch: whether it folds, whether it is the last of its siblings, and which lines carry on past it", () => {
    const seen = readSnapshot(wire());
    const shown = rows(seen, "show");
    const row = (index: number) => shown.find((r) => r.index === index);
    // The application has two children (the window, the menu bar): it branches. Nothing joins it to a parent.
    expect(row(0)).toMatchObject({ depth: 0, parent: null, branch: true, open: true, last: false, lines: [] });
    // The window hangs from it by an elbow; no line passes beside it.
    expect(row(1)).toMatchObject({ depth: 1, parent: 0, branch: true, last: false, lines: [] });
    // Beside the button: the application's line carries on (the menu bar is still to come
    // after the window), the window's doesn't (the group is its only child).
    expect(row(3)).toMatchObject({ depth: 3, parent: 2, branch: false, last: false, lines: [true, false] });
    expect(row(4)).toMatchObject({ depth: 3, parent: 2, branch: false, last: true, lines: [true, false] });
    expect(row(5)).toMatchObject({ depth: 1, parent: 0, branch: false, last: true, lines: [] });
    // With the group hidden, the button hangs from the window.
    expect(rows(seen, "hide").find((r) => r.index === 3)).toMatchObject({ parent: 1, depth: 2, lines: [true] });
  });

  it("folds a branch, hiding what's under it, and opens the ancestors of what is pointed at", () => {
    const seen = readSnapshot(wire());
    const folded = rows(seen, "show", new Set([1]));
    expect(folded.map((r) => r.index)).toEqual([0, 1, 5, 6]);
    expect(folded[1]).toMatchObject({ index: 1, branch: true, open: false });
    const opened = reveal(seen, new Set([0, 1, 2, 5]), 3);
    expect([...opened]).toEqual([5]);
  });

  it("starts with menus folded", () => {
    const menus = readSnapshot(
      create(AxSnapshotSchema, {
        nodes: [
          { role: "AXApplication" },
          { parent: 0, role: "AXMenuBar" },
          { parent: 1, role: "AXMenuBarItem", title: "File" },
          { parent: 0, role: "AXWindow" },
        ],
      }),
    );
    expect([...initiallyFolded(menus)]).toEqual([1]);
  });

  it("finds the deepest element under a point", () => {
    const seen = readSnapshot(wire());
    expect(elementAt(seen, { x: 420, y: 320 }, "show")).toBe(3);
    expect(elementAt(seen, { x: 300, y: 340 }, "show")).toBe(2);
    expect(elementAt(seen, { x: 300, y: 340 }, "hide")).toBe(1);
    expect(elementAt(seen, { x: 10, y: 10 }, "show")).toBe(5);
    expect(elementAt(seen, { x: 900, y: 700 }, "show")).toBeNull();
    expect(ancestors(seen, 3)).toEqual([2, 1, 0]);
  });

  it("classifies roles, and nothing else matches them as text", () => {
    expect(["AXWindow", "AXButton", "AXTextArea", "AXImage", "AXScrollArea", "AXMenuItem", "AXNew"].map(kindOf)).toEqual([
      "window",
      "control",
      "field",
      "text",
      "container",
      "menu",
      "other",
    ]);
  });
});

describe("the snapshot shown on a timeline", () => {
  const look = (request: number) => started(request, Gesture.LOOK, { case: undefined });
  const done = { case: "done", value: {} };
  const timeline = (...events: [ReturnType<typeof started>, number][]): readonly Moment[] =>
    events.reduce<readonly Moment[]>((moments, [event, at]) => record(moments, event, at), []);

  it("is the latest one taken, and says when the agent acted since", () => {
    const moments = timeline(
      [look(1), 0],
      [ended(1, done, wire()), 100],
      [started(2, Gesture.WAIT, { case: undefined }), 200],
      [ended(2, done), 300],
      [started(3, Gesture.CLICK, { case: "point", value: { x: 1, y: 1 } }), 400],
      [ended(3, done), 450],
      [look(4), 500],
      [ended(4, done, { ...wire(), snapshotId: 4n }), 600],
    );
    expect(seenAt(moments, 50)).toBeNull();
    const first = seenAt(moments, 300);
    expect(first?.source.kind === "live" && first.source.seen.snapshot).toBe(3n);
    expect(first?.sinceMs).toBe(100);
    // Waiting doesn't change the screen; a click may.
    expect(first?.actedSince).toBe(false);
    expect(seenAt(moments, 420)?.actedSince).toBe(true);
    const second = seenAt(moments, 700);
    expect(second?.source.kind === "live" && second.source.seen.snapshot).toBe(4n);
    expect(second?.actedSince).toBe(false);
  });

  it("carries the refs the agent gave the snapshot's elements, once it names them", async () => {
    const { create } = await import("@bufbuild/protobuf");
    const { ActivitySchema, ElementRefsSchema } = await import("../gen/doppel/daemon/v1/daemon_pb");
    const { parseActivity } = await import("./overlay");
    const named = (snapshot: bigint) =>
      parseActivity(
        create(ActivitySchema, {
          session: 1n,
          request: 9n,
          machine: "dev",
          phase: { case: "named", value: { snapshot, generation: 4, nodes: [3, 4] } },
        }),
      );
    let moments = timeline([look(1), 0], [ended(1, done, wire()), 100]);
    for (const event of [named(3n), named(8n)]) if (event !== null) moments = record(moments, event, 200);
    const shown = seenAt(moments, 300);
    if (shown?.source.kind !== "live") throw new Error("expected a live snapshot");
    const refs = refsOf(shown.source.seen, shown.moment.refs);
    expect(refs?.get(3)).toBe("s4e1");
    expect(refs?.get(4)).toBe("s4e2");
    expect(refs?.get(1)).toBeUndefined();
    // Names of another snapshot aren't this one's; a generation of 0 isn't one.
    const other = readRefs(create(ElementRefsSchema, { snapshot: 8n, generation: 1, nodes: [3] }));
    expect(refsOf(shown.source.seen, other)).toBeNull();
    expect(readRefs(create(ElementRefsSchema, { snapshot: 3n, generation: 0 }))).toBeNull();
  });
});

describe("the elements panel", () => {
  const look = (request: number) => started(request, Gesture.LOOK, { case: undefined });
  const done = { case: "done", value: {} };

  it("shows the newer of the agent's snapshot and a person's own look", () => {
    let moments: readonly Moment[] = [];
    for (const [event, at] of [[look(1), 0], [ended(1, done, wire()), 100]] as const) moments = record(moments, event, at);
    const mine: Look = { atMs: 200, outcome: { kind: "loading" } };
    expect(showing(moments, null, 300).kind).toBe("agent");
    expect(showing(moments, mine, 300)).toMatchObject({ kind: "person", actedSince: false });
    // A click after the look may have changed the screen.
    moments = record(moments, started(2, Gesture.CLICK, { case: "point", value: { x: 1, y: 1 } }), 400);
    expect(showing(moments, mine, 500)).toMatchObject({ kind: "person", actedSince: true });
    // The agent's next snapshot is newer.
    moments = record(moments, look(3), 600);
    moments = record(moments, ended(3, done, { ...wire(), snapshotId: 9n }), 700);
    expect(showing(moments, mine, 800)).toMatchObject({ kind: "agent", found: { sinceMs: 700 } });
    expect(showing([], null, 0).kind).toBe("nothing");
  });
});

describe("calls from before the page began watching", () => {
  it("are placed back in time with the snapshot the latest one saw, so the panel has it at once", async () => {
    const { create } = await import("@bufbuild/protobuf");
    const { ActivitySchema } = await import("../gen/doppel/daemon/v1/daemon_pb");
    const { parseActivity } = await import("./overlay");
    const recent = (request: number, startedMsAgo: bigint, ended: object | undefined, endedMsAgo: bigint | undefined, gesture: Gesture) =>
      parseActivity(
        create(ActivitySchema, {
          session: 2n,
          request: BigInt(request),
          machine: "dev",
          caption: "a call",
          phase: { case: "recent", value: { started: { gesture }, ended: ended as never, startedMsAgo, endedMsAgo } },
        }),
      );
    const events = [
      recent(1, 9000n, { outcome: { case: "done", value: {} }, seen: wire() }, 8800n, Gesture.LOOK),
      recent(2, 5000n, { outcome: { case: "failed", value: "no window" } }, 4900n, Gesture.CLICK),
      recent(3, 1000n, undefined, undefined, Gesture.WAIT),
    ];
    const now = 100_000;
    let moments: readonly Moment[] = [];
    for (const event of events) if (event !== null) moments = record(moments, event, now);
    expect(moments.map((m) => [m.startMs, m.endMs, m.failed])).toEqual([
      [91_000, 91_200, null],
      [95_000, 95_100, "no window"],
      [99_000, null, null],
    ]);
    const shown = seenAt(moments, now);
    expect(shown?.source.kind === "live" && shown.source.seen.app?.name).toBe("Calculator");
    expect(shown?.actedSince).toBe(true);
    // Told twice (the page reconnected), a call stays once.
    const again = events[0];
    if (again !== null && again !== undefined) expect(record(moments, again, now + 5000)).toHaveLength(3);
  });
});

describe("a tree row's text", () => {
  it("is one short line", () => {
    expect(oneLine("Last login:\n  Fri\tNov 13\n")).toBe("Last login: Fri Nov 13");
    const line = oneLine("word ".repeat(100));
    expect(line.endsWith("…")).toBe(true);
    expect([...line].length).toBe(ONE_LINE_MAX + 1);
    expect(oneLine("short")).toBe("short");
  });
});

describe("what's seen of an element", () => {
  it("is its frame inside every enclosing element's", () => {
    const seen = readSnapshot(
      create(AxSnapshotSchema, {
        nodes: [
          { role: "AXApplication" },
          { parent: 0, role: "AXWindow", frame: frame(100, 300, 800, 400) },
          // A scroll area with no frame bounds nothing more.
          { parent: 1, role: "AXScrollArea" },
          // The terminal's text: its whole scrollback, mostly above the window.
          { parent: 2, role: "AXTextArea", frame: frame(100, -2000, 780, 2700) },
          // Scrolled wholly out of the window.
          { parent: 3, role: "AXStaticText", frame: frame(120, -1500, 100, 20) },
        ],
      }),
    );
    expect(seen.elements[3]?.frame).toEqual({ left: 100, top: -2000, width: 780, height: 2700 });
    expect(seen.elements[3]?.seen).toEqual({ left: 100, top: 300, width: 780, height: 400 });
    expect(seen.elements[4]?.seen).toBeNull();
    expect(seen.elements[2]?.seen).toBeNull();
    // Pointing above the window finds nothing there.
    expect(elementAt(seen, { x: 300, y: 100 }, "show")).toBeNull();
    expect(elementAt(seen, { x: 300, y: 500 }, "show")).toBe(3);
  });
});

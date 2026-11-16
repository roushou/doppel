// An accessibility snapshot as a person reads it: a tree of elements, each
// with its kind, its name and where it is on the display. Read once from
// the wire here; the tree panel and the elements layer only use what this
// module returns. Pure.

import { type AxSnapshot, Truncation } from "../gen/doppel/agent/v1/agent_pb";
import type { ElementRefs } from "../gen/doppel/daemon/v1/daemon_pb";
import type { DisplayPoint, Rect } from "./geometry";

/** What an element is, for its colour and for which ones a person cares about. */
export type Kind = "window" | "control" | "field" | "text" | "container" | "menu" | "other";

/** States an element may report; absent when it doesn't. */
export interface States {
  enabled?: boolean;
  focused?: boolean;
  selected?: boolean;
  expanded?: boolean;
}

/** One element: its place in the tree, and what it is. */
export interface Element {
  /** Its index in the snapshot: the agent acts on it by this. */
  index: number;
  parent: number | null;
  depth: number;
  children: number[];
  /** The raw role (`AXButton`), kept whatever its kind. */
  role: string;
  /** The role in words (`Button`). */
  roleWords: string;
  kind: Kind;
  /** What a person would call it: its title, description or text, or `""`. */
  name: string;
  /** Its value as text, when it has one that isn't its name. */
  value: string | null;
  /** Where it is, in display points, as the app reports it; `null` when it has no area. */
  frame: Rect | null;
  /**
   * The part of its frame inside every enclosing element's: what's on
   * screen of it. A text area's frame is its whole document, scrolled
   * mostly out of its window. `null` when none of it is.
   */
  seen: Rect | null;
  states: States;
  actions: string[];
  identifier: string;
  /** An unnamed container: the agent's listing shows its children in its place. */
  wrapper: boolean;
}

/**
 * A snapshot read: its elements in the snapshot's order (pre-order, the
 * first the root). Each element's parent comes before it, so depths and
 * children are consistent; a node whose parent doesn't is read as a root.
 */
export interface Seen {
  snapshot: bigint;
  app: { name: string; bundleId: string; pid: number } | null;
  elements: Element[];
  /** The agent's snapshot stopped early (too many nodes, too deep, too slow). */
  truncated: boolean;
}

/** What an agent named a snapshot's elements, as its model was shown them: refs like `s3e12`, by element index. */
export interface Refs {
  snapshot: bigint;
  byElement: ReadonlyMap<number, string>;
}

/** The daemon's names for a snapshot's elements; `null` for ones this page can't read. */
export function readRefs(wire: ElementRefs): Refs | null {
  if (wire.generation === 0) return null;
  const byElement = new Map<number, string>();
  wire.nodes.forEach((node, index) => byElement.set(node, `s${wire.generation}e${index + 1}`));
  return { snapshot: wire.snapshot, byElement };
}

/** The refs of `seen`'s elements; `null` when the agent named none, or named another snapshot's. */
export function refsOf(seen: Seen, refs: Refs | null): ReadonlyMap<number, string> | null {
  return refs !== null && refs.snapshot === seen.snapshot ? refs.byElement : null;
}

/**
 * The kind of a raw role. The only place roles are matched as strings. The
 * same table as the Rust viewer's (`Kind::of` over the core's `Role`): a
 * role the core doesn't know is "other" in both.
 */
export function kindOf(role: string): Kind {
  switch (role) {
    case "AXApplication":
    case "AXWindow":
    case "AXSheet":
    case "AXPopover":
      return "window";
    case "AXButton":
    case "AXCheckBox":
    case "AXRadioButton":
    case "AXPopUpButton":
    case "AXMenuButton":
    case "AXLink":
    case "AXSlider":
    case "AXIncrementor":
    case "AXDisclosureTriangle":
    case "AXColorWell":
      return "control";
    case "AXComboBox":
    case "AXTextField":
    case "AXTextArea":
      return "field";
    case "AXStaticText":
    case "AXImage":
    case "AXHeading":
    case "AXProgressIndicator":
    case "AXBusyIndicator":
    case "AXValueIndicator":
    case "AXLevelIndicator":
      return "text";
    case "AXToolbar":
    case "AXGroup":
    case "AXScrollArea":
    case "AXSplitGroup":
    case "AXSplitter":
    case "AXScrollBar":
    case "AXLayoutArea":
    case "AXLayoutItem":
    case "AXTabGroup":
    case "AXWebArea":
    case "AXList":
    case "AXTable":
    case "AXOutline":
    case "AXBrowser":
    case "AXRow":
    case "AXColumn":
    case "AXCell":
    case "AXRuler":
    case "AXRulerMarker":
      return "container";
    case "AXMenuBar":
    case "AXMenuBarItem":
    case "AXMenu":
    case "AXMenuItem":
      return "menu";
    default:
      // A role this page doesn't know: shown as it is, never dropped.
      return "other";
  }
}

/** The name a subrole gives an element that has none of its own (the window's buttons). */
function impliedName(subrole: string): string {
  switch (subrole) {
    case "AXCloseButton":
      return "close";
    case "AXMinimizeButton":
      return "minimize";
    case "AXZoomButton":
      return "zoom";
    case "AXFullScreenButton":
      return "full screen";
    default:
      return "";
  }
}

/** The most of a name or value a tree row shows. */
export const ONE_LINE_MAX = 120;

/**
 * `text` as one line for a tree row: its runs of spaces and line breaks
 * each one space, cut with "…" past `ONE_LINE_MAX` characters. A text
 * area's value is its whole contents, many lines of it.
 */
export function oneLine(text: string): string {
  const line = text.split(/\s+/).filter((word) => word !== "").join(" ");
  const chars = [...line];
  return chars.length > ONE_LINE_MAX ? `${chars.slice(0, ONE_LINE_MAX).join("")}…` : line;
}

/** Where an element's children can be seen: anywhere, within a rectangle, or nowhere. */
type Bound = { kind: "anywhere" } | { kind: "within"; rect: Rect } | { kind: "nowhere" };

/** The bound inside `outer` and `frame` (no frame bounds nothing more). */
function within(outer: Bound, frame: Rect | null): Bound {
  if (frame === null) return outer;
  switch (outer.kind) {
    case "anywhere":
      return { kind: "within", rect: frame };
    case "nowhere":
      return outer;
    case "within": {
      const left = Math.max(outer.rect.left, frame.left);
      const top = Math.max(outer.rect.top, frame.top);
      const right = Math.min(outer.rect.left + outer.rect.width, frame.left + frame.width);
      const bottom = Math.min(outer.rect.top + outer.rect.height, frame.top + frame.height);
      return right > left && bottom > top ? { kind: "within", rect: { left, top, width: right - left, height: bottom - top } } : { kind: "nowhere" };
    }
    default: {
      const never: never = outer;
      return never;
    }
  }
}

function seenOf(bound: Bound): Rect | null {
  switch (bound.kind) {
    case "within":
      return bound.rect;
    case "anywhere":
    case "nowhere":
      return null;
    default: {
      const never: never = bound;
      return never;
    }
  }
}

/** `AXPopUpButton` as `Pop Up Button`. */
function words(role: string): string {
  const bare = role.startsWith("AX") ? role.slice(2) : role;
  return bare.replace(/([a-z])([A-Z])/g, "$1 $2") || role;
}

function text(value: AxSnapshot["nodes"][number]["value"]): string | null {
  const inner = value?.value;
  switch (inner?.case) {
    case "text":
      return inner.value;
    case "number":
      return String(inner.value);
    case "boolean":
      return inner.value ? "on" : "off";
    case undefined:
      return null;
    default: {
      const never: never = inner;
      return never;
    }
  }
}

function area(frame: AxSnapshot["nodes"][number]["frame"]): Rect | null {
  const origin = frame?.origin;
  const size = frame?.size;
  if (origin === undefined || size === undefined) return null;
  const all = [origin.x, origin.y, size.width, size.height];
  if (!all.every(Number.isFinite) || size.width <= 0 || size.height <= 0) return null;
  return { left: origin.x, top: origin.y, width: size.width, height: size.height };
}

/** A snapshot from the wire, read into a tree. */
export function readSnapshot(wire: AxSnapshot): Seen {
  const elements: Element[] = [];
  /** What each element's children are seen within. */
  const bounds: Bound[] = [];
  wire.nodes.forEach((node, index) => {
    const parent = node.parent !== undefined && node.parent < index ? node.parent : null;
    const above = parent === null ? undefined : elements[parent];
    const frame = area(node.frame);
    const bound = within(parent === null ? { kind: "anywhere" } : (bounds[parent] ?? { kind: "anywhere" }), frame);
    bounds.push(bound);
    const valueText = text(node.value);
    // As the core names it (`AxNode::name`), which the agent's listing uses too.
    const said = node.role === "AXStaticText" && node.value?.value.case === "text" ? node.value.value.value : "";
    const name = [node.title, node.description, said].map((candidate) => candidate.trim()).find((candidate) => candidate !== "") ?? impliedName(node.subrole);
    const kind = kindOf(node.role);
    const flags = node.flags;
    const states: States = {};
    if (flags?.enabled !== undefined) states.enabled = flags.enabled;
    if (flags?.focused !== undefined) states.focused = flags.focused;
    if (flags?.selected !== undefined) states.selected = flags.selected;
    if (flags?.expanded !== undefined) states.expanded = flags.expanded;
    elements.push({
      index,
      parent,
      depth: above === undefined ? 0 : above.depth + 1,
      children: [],
      role: node.role,
      roleWords: words(node.role),
      kind,
      name,
      value: valueText === null || valueText.trim() === "" || valueText.trim() === name ? null : valueText,
      frame,
      seen: frame === null ? null : seenOf(bound),
      states,
      actions: [...node.actions],
      identifier: node.identifier,
      wrapper: kind === "container" && name === "",
    });
    above?.children.push(index);
  });
  return {
    snapshot: wire.snapshotId,
    app: wire.app === undefined ? null : { name: wire.app.name, bundleId: wire.app.bundleId, pid: wire.app.pid },
    elements,
    truncated: truncated(wire.truncation),
  };
}

function truncated(truncation: Truncation): boolean {
  switch (truncation) {
    case Truncation.COMPLETE:
    case Truncation.UNSPECIFIED:
      return false;
    case Truncation.NODE_BUDGET:
    case Truncation.DEPTH_BUDGET:
    case Truncation.DEADLINE:
      return true;
    default:
      // A reason a newer agent added: it still stopped early.
      return true;
  }
}

/** A row of the tree panel: an element, where it sits in the tree, and how to draw its branch. */
export interface Row {
  index: number;
  depth: number;
  /** The row's parent in the tree shown (a wrapper hidden, its parent's); `null` at the top. */
  parent: number | null;
  /** It has children shown under it, so it can fold. */
  branch: boolean;
  /** Its children are shown. */
  open: boolean;
  /** The last of its siblings: its branch line stops here. */
  last: boolean;
  /**
   * The lines passing beside it, one per column from the left, under each
   * ancestor above its parent: whether that ancestor's line carries on
   * past this row, because the ancestor's child on the way down here has
   * siblings still to come. Its own link to its parent is the elbow at
   * column `depth - 1`.
   */
  lines: boolean[];
}

/** An element's children as the tree shows them: with wrappers hidden, a wrapper's children in its place. */
export function shownChildren(seen: Seen, index: number, wrappers: "show" | "hide"): number[] {
  const element = seen.elements[index];
  if (element === undefined) return [];
  return element.children.flatMap((child) =>
    wrappers === "hide" && seen.elements[child]?.wrapper === true ? shownChildren(seen, child, wrappers) : [child],
  );
}

/** The elements at the top of the tree shown. */
function roots(seen: Seen, wrappers: "show" | "hide"): number[] {
  return seen.elements
    .filter((element) => element.parent === null)
    .flatMap((element) => (wrappers === "hide" && element.wrapper ? shownChildren(seen, element.index, wrappers) : [element.index]));
}

/**
 * The rows the tree panel shows, in order. With wrappers hidden, an
 * unnamed container's children take its place, one level up, as in the
 * agent's listing. A folded element's descendants aren't shown.
 */
export function rows(seen: Seen, wrappers: "show" | "hide", folded: ReadonlySet<number> = new Set()): Row[] {
  const out: Row[] = [];
  const visit = (index: number, parent: number | null, depth: number, lines: boolean[], last: boolean) => {
    const children = shownChildren(seen, index, wrappers);
    const open = !folded.has(index);
    out.push({ index, depth, parent, branch: children.length > 0, open, last, lines });
    if (!open) return;
    // A top element has no line of its own to pass on: nothing joins it to a parent.
    const below = depth === 0 ? [] : [...lines, !last];
    children.forEach((child, at) => visit(child, index, depth + 1, below, at === children.length - 1));
  };
  const top = roots(seen, wrappers);
  top.forEach((index, at) => visit(index, null, 0, [], at === top.length - 1));
  return out;
}

/** The folds with `index`'s ancestors opened, so it shows. */
export function reveal(seen: Seen, folded: ReadonlySet<number>, index: number): Set<number> {
  const next = new Set(folded);
  for (const ancestor of ancestors(seen, index)) next.delete(ancestor);
  return next;
}

/** The elements folded at first: menu bars and menus, which are long and rarely what the agent looked at. */
export function initiallyFolded(seen: Seen): Set<number> {
  return new Set(
    seen.elements.filter((element) => element.kind === "menu" && element.children.length > 0).map((element) => element.index),
  );
}

function contains(frame: Rect, at: DisplayPoint): boolean {
  return at.x >= frame.left && at.x <= frame.left + frame.width && at.y >= frame.top && at.y <= frame.top + frame.height;
}

/**
 * The element under a display point: the deepest one whose frame holds it,
 * the smallest of those on a tie, and the latest in the snapshot (drawn in
 * front) when they are the same size too. Wrappers are passed over when
 * `wrappers` hides them. `null` when none does.
 */
export function elementAt(seen: Seen, at: DisplayPoint, wrappers: "show" | "hide"): number | null {
  let best: Element | null = null;
  for (const element of seen.elements) {
    if (element.seen === null || !contains(element.seen, at)) continue;
    if (wrappers === "hide" && element.wrapper) continue;
    if (
      best === null ||
      element.depth > best.depth ||
      (element.depth === best.depth && element.seen.width * element.seen.height <= area2(best))
    ) {
      best = element;
    }
  }
  return best?.index ?? null;
}

function area2(element: Element): number {
  return element.seen === null ? Infinity : element.seen.width * element.seen.height;
}

/** An element's ancestors, nearest first. */
export function ancestors(seen: Seen, index: number): number[] {
  const out: number[] = [];
  let parent = seen.elements[index]?.parent ?? null;
  while (parent !== null && !out.includes(parent)) {
    out.push(parent);
    parent = seen.elements[parent]?.parent ?? null;
  }
  return out;
}

// What the page keeps about each machine: the action a person started on it,
// whether this page has control of it, its report, and what agents did on
// it. One `Panel` per machine, in `Panels`, as the desktop viewer keeps its
// own. Pure: each change returns new panels, for the page to keep.
//
// A panel's lens and the person's own looks stay in Panel.svelte, with the
// screen they are drawn over.

import type { Action } from "./lifecycle";
import type { MachineName } from "./machines";
import type { ActivityEvent } from "./overlay";
import type { Report } from "./report";
import { type Moment, record } from "./timeline";

/** Whether this page holds a machine's control. */
export type Control = "held" | "free";

/** Everything the page keeps about one machine. */
export interface Panel {
  /** The action a person started, until its verdict. */
  readonly action: Action | null;
  readonly control: Control;
  /** Its latest report, while it is on the stage. */
  readonly report: Report | null;
  /** What agents did on it since the page began watching. */
  readonly timeline: readonly Moment[];
}

export type Panels = ReadonlyMap<MachineName, Panel>;

const EMPTY: Panel = { action: null, control: "free", report: null, timeline: [] };

/** `name`'s panel; empty if nothing is kept about it. */
export function panelOf(panels: Panels, name: MachineName): Panel {
  return panels.get(name) ?? EMPTY;
}

function changed(panels: Panels, name: MachineName, change: (panel: Panel) => Panel): Panels {
  return new Map(panels).set(name, change(panelOf(panels, name)));
}

/** Starts `action` on `name`, or `null` while another is under way there. */
export function begin(panels: Panels, name: MachineName, action: Action): Panels | null {
  if (panelOf(panels, name).action !== null) return null;
  return changed(panels, name, (panel) => ({ ...panel, action }));
}

/** The action under way on `name` has its verdict. */
export function finish(panels: Panels, name: MachineName): Panels {
  return changed(panels, name, (panel) => ({ ...panel, action: null }));
}

/** This page took control of `name`. */
export function took(panels: Panels, name: MachineName): Panels {
  return changed(panels, name, (panel) => ({ ...panel, control: "held" }));
}

/** This page no longer has control of `name`. */
export function handedBack(panels: Panels, name: MachineName): Panels {
  return changed(panels, name, (panel) => ({ ...panel, control: "free" }));
}

/** The machines this page has control of. */
export function held(panels: Panels): MachineName[] {
  return [...panels].flatMap(([name, panel]) => (panel.control === "held" ? [name] : []));
}

/** The machine an activity event is about. */
export function machineOf(event: ActivityEvent): MachineName {
  switch (event.kind) {
    case "started":
    case "recent":
      return event.call.machine;
    case "ended":
    case "named":
      return event.machine;
    default: {
      const never: never = event;
      return never;
    }
  }
}

/** Records what an agent did at `now`, on its machine's timeline. */
export function recorded(panels: Panels, event: ActivityEvent, now: number): Panels {
  return changed(panels, machineOf(event), (panel) => ({ ...panel, timeline: record(panel.timeline, event, now) }));
}

/**
 * The answer to reading `name`'s report: kept while `name` is in `staged`;
 * `null`, when the read failed, drops the one kept.
 */
export function reported(panels: Panels, name: MachineName, report: Report | null, staged: readonly MachineName[]): Panels {
  if (!staged.includes(name) && panelOf(panels, name).report === null) return panels;
  return changed(panels, name, (panel) => ({ ...panel, report: staged.includes(name) ? report : null }));
}

/** Drops the reports of machines no longer in `staged`. */
export function keepReportsOf(panels: Panels, staged: readonly MachineName[]): Panels {
  const next = new Map(panels);
  for (const [name, panel] of panels) if (!staged.includes(name) && panel.report !== null) next.set(name, { ...panel, report: null });
  return next;
}

/** The connection closed: control is lost with it, and no action is under way any more. */
export function disconnected(panels: Panels): Panels {
  return new Map([...panels].map(([name, panel]) => [name, { ...panel, action: null, control: "free" }]));
}

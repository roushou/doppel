// What a person can do to a machine, from the state the daemon reports:
// the page offers only actions that state allows, and the daemon still
// refuses any that no longer fit by the time they arrive.

import { type Machine, type MachineName, type MachineState, template } from "./machines";
import type { Label } from "./operate";

export type Action =
  | "start"
  /** Start a golden image, once the person has heard it drifts from its template. */
  | "start-golden"
  /** A new machine from a golden image's template. */
  | "from-template"
  | "stop"
  | "force-stop"
  | "pause"
  | "resume"
  | "checkpoint"
  | "roll-back"
  | "restore"
  | "fork"
  | "new-copy"
  | "delete";

/**
 * The actions a machine allows, in the order a menu shows them. A stopped
 * golden image is for making new machines: that comes first, and starting
 * it, which lets it drift from its template, asks first.
 */
export function machineActions(machine: Machine): Action[] {
  if (machine.state.kind === "stopped" && template(machine) !== null) {
    return ["from-template", "fork", "restore", "start-golden", "delete"];
  }
  return actions(machine.state);
}

/** Actions shown as buttons in a panel's head; the rest go in its menu. */
export function shown(action: Action): boolean {
  switch (action) {
    case "start":
    case "from-template":
    case "resume":
    case "pause":
    case "checkpoint":
      return true;
    case "start-golden":
    case "stop":
    case "force-stop":
    case "roll-back":
    case "restore":
    case "fork":
    case "new-copy":
    case "delete":
      return false;
    default: {
      const never: never = action;
      return never;
    }
  }
}

/** The actions a machine in `state` allows, in the order a menu shows them. */
export function actions(state: MachineState): Action[] {
  switch (state.kind) {
    case "stopped":
      return ["start", "new-copy", "fork", "restore", "delete"];
    case "unreadable":
      // A machine whose status can't be read can still be cleared away.
      return ["delete"];
    case "live":
      switch (state.run) {
        case "running":
          return ["pause", "stop", "checkpoint", "roll-back", "force-stop"];
        case "paused":
          return ["resume", "checkpoint", "force-stop"];
        case "starting":
        case "busy":
        case "stopping":
        case "error":
        case "unknown":
          return ["force-stop"];
        default: {
          const never: never = state.run;
          return never;
        }
      }
    default: {
      const never: never = state;
      return never;
    }
  }
}

/** Actions that need a checkpoint chosen first. */
export function needsCheckpoint(action: Action): boolean {
  switch (action) {
    case "roll-back":
    case "restore":
    case "fork":
    case "new-copy":
    case "from-template":
      return true;
    case "start":
    case "start-golden":
    case "stop":
    case "force-stop":
    case "pause":
    case "resume":
    case "checkpoint":
    case "delete":
      return false;
    default: {
      const never: never = action;
      return never;
    }
  }
}

/** What a machine shows while an action on it is under way. */
export function underWay(action: Action): string {
  switch (action) {
    case "start":
    case "start-golden":
      return "Starting…";
    case "from-template":
      return "Making a machine…";
    case "stop":
    case "force-stop":
      return "Stopping…";
    case "pause":
      return "Pausing…";
    case "resume":
      return "Resuming…";
    case "checkpoint":
      return "Saving a checkpoint…";
    case "roll-back":
      return "Rolling back…";
    case "restore":
      return "Restoring…";
    case "fork":
      return "Forking…";
    case "new-copy":
      return "Making a copy…";
    case "delete":
      return "Deleting…";
    default: {
      const never: never = action;
      return never;
    }
  }
}

/** The checkpoint chosen once a machine's are read: a golden image's template when it's among them, else the newest. */
export function preselected(labels: Label[], template: Label | null): Label | null {
  return template !== null && labels.includes(template) ? template : (labels.at(-1) ?? null);
}

/** A checkpoint label the daemon takes: 1 to 64 of lowercase letters, digits, `-` and `_`. */
export function checkpointLabel(raw: string): Label | null {
  const label = raw.trim();
  return /^[a-z0-9_-]{1,64}$/.test(label) ? (label as Label) : null;
}

/** A machine name the daemon takes: lowercase letters, digits and `-`, not starting with `-`. */
export function machineName(raw: string): MachineName | null {
  const name = raw.trim();
  return name.length <= MAX_NAME && /^[a-z0-9][a-z0-9-]*$/.test(name) ? (name as MachineName) : null;
}

/** The longest machine name the daemon takes. */
const MAX_NAME = 32;

/** The colour family a machine's state is shown in. */
export type Tone = "running" | "paused" | "busy" | "stopped" | "error";

/** How a machine's state reads at a glance: a word and its colour. */
export function look(state: MachineState): { label: string; tone: Tone } {
  switch (state.kind) {
    case "stopped":
      return { label: "Stopped", tone: "stopped" };
    case "unreadable":
      return { label: "Unreadable", tone: "error" };
    case "live":
      switch (state.run) {
        case "running":
          return { label: "Running", tone: "running" };
        case "paused":
          return { label: "Paused", tone: "paused" };
        case "starting":
          return { label: "Starting", tone: "busy" };
        case "busy":
          return { label: "Busy", tone: "busy" };
        case "stopping":
          return { label: "Stopping", tone: "busy" };
        case "error":
          return { label: "Error", tone: "error" };
        case "unknown":
          return { label: "Unknown", tone: "error" };
        default: {
          const never: never = state.run;
          return never;
        }
      }
    default: {
      const never: never = state;
      return never;
    }
  }
}

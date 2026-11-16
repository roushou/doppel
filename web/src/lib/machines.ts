// The daemon's machine list, parsed once into what the page shows: each
// machine's name, whether it has a screen to watch, and what it's for.

import type { Lineage as WireLineage, MachineEntry, MachineList } from "../gen/doppel/daemon/v1/daemon_pb";
import { VmRunState } from "../gen/doppel/vmm/v1/vmm_pb";
import { checkpointLabel, machineName } from "./lifecycle";
import type { Label } from "./operate";

declare const brand: unique symbol;

/** A machine's name, as the daemon lists it. */
export type MachineName = string & { readonly [brand]: "MachineName" };

/** Where a machine is: a live one has a screen. */
export type MachineState =
  | { kind: "stopped" }
  | { kind: "live"; run: "starting" | "running" | "paused" | "busy" | "stopping" | "error" | "unknown" }
  | { kind: "unreadable"; reason: string };

/** What a machine is for: one to use, or a template whose new machines start from `template`. */
export type Role = { kind: "working" } | { kind: "golden"; template: Label };

/** How a machine came to be, as recorded when it was made. `unrecorded`: made before that, unknown. */
export type Origin =
  | { kind: "unrecorded" }
  | { kind: "installed" }
  | { kind: "imported" }
  | { kind: "forked"; from: MachineName; checkpoint: Label };

/** A machine's role and origin, or why they couldn't be read: then it's neither golden nor working. */
export type Lineage = { kind: "known"; role: Role; origin: Origin } | { kind: "unreadable"; reason: string };

/** What a machine that recorded nothing is: one to use, of unknown origin. */
export const UNRECORDED: Lineage = { kind: "known", role: { kind: "working" }, origin: { kind: "unrecorded" } };

export interface Machine {
  name: MachineName;
  state: MachineState;
  lineage: Lineage;
}

/** The checkpoint new machines start from, when it's a golden image. */
export function template(machine: Machine): Label | null {
  return machine.lineage.kind === "known" && machine.lineage.role.kind === "golden" ? machine.lineage.role.template : null;
}

/** The switcher's groups: golden images apart, whatever their state. */
export type Group = "running" | "stopped" | "golden";

export function group(machine: Machine): Group {
  if (template(machine) !== null) return "golden";
  return machine.state.kind === "live" ? "running" : "stopped";
}

/** The machines recorded as forked from `name`, in list order. */
export function forksOf(machines: readonly Machine[], name: MachineName): MachineName[] {
  return machines.flatMap((machine) =>
    machine.lineage.kind === "known" && machine.lineage.origin.kind === "forked" && machine.lineage.origin.from === name
      ? [machine.name]
      : [],
  );
}

/** What a panel says about where a machine comes from, if anything. */
export type Provenance = { kind: "said"; text: string } | { kind: "unreadable"; reason: string };

export function provenance(machine: Machine): Provenance | null {
  const lineage = machine.lineage;
  switch (lineage.kind) {
    case "unreadable":
      return { kind: "unreadable", reason: lineage.reason };
    case "known":
      if (lineage.role.kind === "golden") return { kind: "said", text: `Template · ${lineage.role.template}` };
      switch (lineage.origin.kind) {
        case "forked":
          return { kind: "said", text: `From ${lineage.origin.from} · ${lineage.origin.checkpoint}` };
        case "unrecorded":
        case "installed":
        case "imported":
          return null;
        default: {
          const never: never = lineage.origin;
          return never;
        }
      }
    default: {
      const never: never = lineage;
      return never;
    }
  }
}

/** Parses a lineage. A role or origin this page doesn't know is unreadable, never a default. */
function parseLineage(wire: WireLineage): Lineage {
  const unreadable = (reason: string): Lineage => ({ kind: "unreadable", reason });
  let role: Role;
  switch (wire.role.case) {
    case "working":
      role = { kind: "working" };
      break;
    case "golden": {
      const label = checkpointLabel(wire.role.value.checkpoint);
      if (label === null) return unreadable(`a golden image's template isn't a checkpoint label: ${wire.role.value.checkpoint}`);
      role = { kind: "golden", template: label };
      break;
    }
    case undefined:
      return unreadable("a role this page doesn't know");
    default: {
      const never: never = wire.role;
      return never;
    }
  }
  let origin: Origin;
  switch (wire.origin.case) {
    case "unrecorded":
    case "installed":
    case "imported":
      origin = { kind: wire.origin.case };
      break;
    case "forked": {
      const from = machineName(wire.origin.value.machine);
      const checkpoint = checkpointLabel(wire.origin.value.checkpoint);
      if (from === null || checkpoint === null) return unreadable("a fork's source isn't a machine and checkpoint");
      origin = { kind: "forked", from, checkpoint };
      break;
    }
    case undefined:
      return unreadable("an origin this page doesn't know");
    default: {
      const never: never = wire.origin;
      return never;
    }
  }
  return { kind: "known", role, origin };
}

function lineage(entry: MachineEntry): Lineage {
  switch (entry.recordedLineage.case) {
    case "lineage":
      return parseLineage(entry.recordedLineage.value);
    case "lineageUnreadable":
      return { kind: "unreadable", reason: entry.recordedLineage.value };
    case undefined:
      // An older daemon: nothing recorded.
      return UNRECORDED;
    default: {
      const never: never = entry.recordedLineage;
      return never;
    }
  }
}

function run(state: VmRunState): Extract<MachineState, { kind: "live" }>["run"] {
  switch (state) {
    case VmRunState.STARTING:
      return "starting";
    case VmRunState.RUNNING:
      return "running";
    case VmRunState.PAUSED:
      return "paused";
    case VmRunState.PAUSING:
    case VmRunState.RESUMING:
    case VmRunState.SAVING:
    case VmRunState.RESTORING:
      return "busy";
    case VmRunState.STOPPING:
    case VmRunState.STOPPED:
      return "stopping";
    case VmRunState.ERROR:
      return "error";
    case VmRunState.UNSPECIFIED:
      return "unknown";
    default:
      // A state a newer daemon added.
      return "unknown";
  }
}

export function parseMachines(list: MachineList): Machine[] {
  return list.machines.map((entry) => ({ ...parseState(entry), lineage: lineage(entry) }));
}

function parseState(entry: MachineEntry): Omit<Machine, "lineage"> {
  const name = entry.name as MachineName;
  switch (entry.status.case) {
    case "machineStatus": {
      const kind = entry.status.value.kind;
      switch (kind.case) {
        case "stopped":
          return { name, state: { kind: "stopped" } };
        case "live":
          return { name, state: { kind: "live", run: run(kind.value.runState) } };
        case undefined:
          return { name, state: { kind: "unreadable", reason: "a status this page doesn't know" } };
        default: {
          const never: never = kind;
          return never;
        }
      }
    }
    case "failure":
      return { name, state: { kind: "unreadable", reason: entry.status.value.message } };
    case undefined:
      return { name, state: { kind: "unreadable", reason: "no status" } };
    default: {
      const never: never = entry.status;
      return never;
    }
  }
}

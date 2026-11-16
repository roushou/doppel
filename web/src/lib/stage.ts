// The machines' slots: two, because Apple runs at most two macOS machines
// at a time. A slot holds a live machine or is empty; an empty slot is
// where a machine is started. The stage shows one slot's machine, or both
// side by side. Pure: the machines and the slots as last kept in, what to
// show out. (A slot is a "bay" here.)

import { machineName } from "./lifecycle";
import type { Machine, MachineName } from "./machines";

/** How many machines can run at once: Apple's limit, which doppeld enforces. */
export const BAYS = 2;

export type Bay = { kind: "machine"; machine: Machine } | { kind: "empty"; index: number };

/** What each bay was given, by position: a name, or nothing. */
export type Kept = readonly (MachineName | null)[];

function live(machine: Machine): boolean {
  return machine.state.kind === "live";
}

/**
 * The bays. A live machine keeps the bay it had; one newly live takes the
 * bay it was started from, else the first empty one. A bay whose machine
 * stopped is empty again.
 */
export function bays(machines: readonly Machine[], kept: Kept): Bay[] {
  const placed: (Machine | null)[] = Array.from({ length: BAYS }, (_, index) => {
    const name = kept[index] ?? null;
    return machines.find((machine) => machine.name === name && live(machine)) ?? null;
  });
  for (const machine of machines.filter(live)) {
    if (placed.includes(machine)) continue;
    const own = placed.findIndex((held, index) => held === null && kept[index] === machine.name);
    const free = own >= 0 ? own : placed.indexOf(null);
    if (free >= 0) placed[free] = machine;
  }
  return placed.map((machine, index) => (machine === null ? { kind: "empty", index } : { kind: "machine", machine }));
}

/** The bays as kept: what each holds, or the machine it was started for. */
export function keep(shown: readonly Bay[], kept: Kept): (MachineName | null)[] {
  return shown.map((bay, index) => (bay.kind === "machine" ? bay.machine.name : (kept[index] ?? null)));
}

/** The bays kept with `name` started from bay `index`: it lands there when it's live. */
export function startIn(kept: Kept, index: number, name: MachineName): (MachineName | null)[] {
  return Array.from({ length: BAYS }, (_, at) => (at === index ? name : (kept[at] ?? null)));
}

/** Whether every bay holds a machine: nothing more can start. */
export function full(shown: readonly Bay[]): boolean {
  return shown.every((bay) => bay.kind === "machine");
}

/** The bays as kept between loads: one line each, empty for none. */
export function keepBays(kept: Kept): string {
  return kept.map((name) => name ?? "").join("\n");
}

/** Kept bays, read back: what isn't a machine name is an empty bay. */
export function readBays(text: string): (MachineName | null)[] {
  const lines = text.split("\n");
  return Array.from({ length: BAYS }, (_, index) => machineName(lines[index] ?? ""));
}

/**
 * The machines on the stage: the picked slot's machine alone, or, split,
 * both. A picked slot with no machine shows the other's; none, nothing.
 */
export function onStage(shown: readonly Bay[], picked: number, split: boolean): Machine[] {
  const machines = shown.flatMap((bay) => (bay.kind === "machine" ? [bay.machine] : []));
  if (split && machines.length === BAYS) return machines;
  const chosen = shown[picked];
  if (chosen?.kind === "machine") return [chosen.machine];
  return machines.slice(0, 1);
}

/** Whether both bays' machines show side by side, when both run. */
export type Split = "on" | "off";

/** How the stage is laid out. A slot is a bay's position in the header. */
export interface Stage {
  /** What each bay was given: the machine in it, or the one started there. */
  readonly kept: readonly (MachineName | null)[];
  /** The slot whose machine the stage shows. */
  readonly picked: number;
  readonly split: Split;
  /** The free slot whose launcher is open, if one is. */
  readonly launching: number | null;
}

/** A stage whose bays were given `kept`. */
export function stageOf(kept: Kept): Stage {
  return { kept: Array.from({ length: BAYS }, (_, index) => kept[index] ?? null), picked: 0, split: "off", launching: null };
}

/** Shows slot `index`'s machine alone. */
export function showSlot(stage: Stage, index: number): Stage {
  return { ...stage, picked: index, split: "off", launching: null };
}

/** Starts `name` from slot `index`: shown there alone, it lands in that bay once it's live. */
export function startFrom(stage: Stage, index: number, name: MachineName): Stage {
  return showSlot({ ...stage, kept: startIn(stage.kept, index, name) }, index);
}

/** Keeps each bay's machine as `shown`, so a machine keeps its bay once it's live; the same stage if nothing moved. */
export function settle(stage: Stage, shown: readonly Bay[]): Stage {
  const next = keep(shown, stage.kept);
  return next.every((name, index) => name === stage.kept[index]) ? stage : { ...stage, kept: next };
}

/** Opens slot `index`'s launcher, or closes it if it's open. */
export function toggleLauncher(stage: Stage, index: number): Stage {
  return { ...stage, launching: stage.launching === index ? null : index };
}

/** Closes the open launcher. */
export function closeLauncher(stage: Stage): Stage {
  return stage.launching === null ? stage : { ...stage, launching: null };
}

export function toggleSplit(stage: Stage): Stage {
  return { ...stage, split: stage.split === "on" ? "off" : "on" };
}

/** What Escape is for, besides the stage: a dialog, or a screen someone drives. */
export type Escaping = "stage" | "elsewhere";

/** Escape closes the open launcher first; then, unless it's meant elsewhere, leaves the split. */
export function escape(stage: Stage, meant: Escaping): Stage {
  if (stage.launching !== null) return closeLauncher(stage);
  if (meant === "stage" && stage.split === "on") return { ...stage, split: "off" };
  return stage;
}

/** The machines on this stage among `shown`. */
export function shows(stage: Stage, shown: readonly Bay[]): Machine[] {
  return onStage(shown, stage.picked, stage.split === "on");
}

/** The slot holding `name` among `shown`, if one does. */
export function slotOf(shown: readonly Bay[], name: MachineName): number | null {
  const at = shown.findIndex((bay) => bay.kind === "machine" && bay.machine.name === name);
  return at < 0 ? null : at;
}

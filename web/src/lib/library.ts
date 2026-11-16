// The recordings list as a library: sessions grouped by the day they
// started, newest first. Pure: "today" is passed in.

import type { RecordingSummary } from "./recordings";

/** Which day a group is, relative to today. */
export type Day = { kind: "today" } | { kind: "yesterday" } | { kind: "on"; date: Date };

export interface DayGroup {
  /** The day's local midnight, as ms: unique per group. */
  key: number;
  day: Day;
  recordings: RecordingSummary[];
}

function midnight(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

/** `recordings` by local day, newest day and newest recording first. */
export function byDay(recordings: readonly RecordingSummary[], today: Date): DayGroup[] {
  const todayKey = midnight(today);
  const yesterdayKey = midnight(new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1));
  const groups = new Map<number, RecordingSummary[]>();
  for (const recording of recordings) {
    const key = midnight(recording.started);
    const group = groups.get(key);
    if (group === undefined) groups.set(key, [recording]);
    else group.push(recording);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => b - a)
    .map(([key, list]) => ({
      key,
      day:
        key === todayKey
          ? { kind: "today" }
          : key === yesterdayKey
            ? { kind: "yesterday" }
            : { kind: "on", date: new Date(key) },
      recordings: [...list].sort((a, b) => b.started.getTime() - a.started.getTime()),
    }));
}

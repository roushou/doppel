import { describe, expect, it } from "vitest";
import { byDay } from "./library";
import type { RecordingId, RecordingSummary } from "./recordings";

const at = (id: string, started: Date): RecordingSummary => ({
  id: id as RecordingId,
  started,
  session: 1n,
  machines: ["dev"],
  videos: 1,
  calls: 1,
  tracedMs: 1000,
});

describe("byDay", () => {
  const today = new Date(2026, 10, 15, 22, 0);

  it("groups by local day, newest day and recording first", () => {
    const groups = byDay(
      [
        at("a", new Date(2026, 10, 15, 9, 0)),
        at("b", new Date(2026, 10, 13, 12, 0)),
        at("c", new Date(2026, 10, 15, 16, 0)),
        at("d", new Date(2026, 10, 14, 23, 59)),
      ],
      today,
    );
    expect(groups.map((g) => g.day.kind)).toEqual(["today", "yesterday", "on"]);
    expect(groups[0]?.recordings.map((r) => r.id)).toEqual(["c", "a"]);
    expect(groups[2]?.day).toEqual({ kind: "on", date: new Date(2026, 10, 13) });
  });

  it("knows yesterday across a month's end", () => {
    const groups = byDay([at("a", new Date(2026, 10, 30, 8))], new Date(2026, 11, 1, 8));
    expect(groups[0]?.day.kind).toBe("yesterday");
  });

  it("is empty for no recordings", () => {
    expect(byDay([], today)).toEqual([]);
  });
});

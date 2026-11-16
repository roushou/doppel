import { describe, expect, it } from "vitest";
import { MAX_QUEUED, step } from "./decoder";

describe("feeding the screen's decoder", () => {
  it("skips delta frames until a key frame configures it", () => {
    expect(step(null, null, 0)).toEqual({ kind: "skip" });
    expect(step("a", null, 0)).toEqual({ kind: "decode", configure: true });
    expect(step(null, "a", 0)).toEqual({ kind: "decode", configure: false });
  });

  it("configures again only when a key frame's parameters change", () => {
    expect(step("a", "a", 0)).toEqual({ kind: "decode", configure: false });
    expect(step("b", "a", 0)).toEqual({ kind: "decode", configure: true });
  });

  it("catches up once too many frames wait, whatever arrives", () => {
    expect(step(null, "a", MAX_QUEUED - 1)).toEqual({ kind: "decode", configure: false });
    expect(step(null, "a", MAX_QUEUED)).toEqual({ kind: "catch-up" });
    expect(step("a", "a", MAX_QUEUED)).toEqual({ kind: "catch-up" });
  });
});

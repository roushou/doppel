import { describe, expect, it } from "vitest";
import { HUES, contrast, hue } from "./hues";

describe("the sessions' colours", () => {
  it("keep captions readable and pointers visible", () => {
    for (const [fill, ink] of HUES) {
      // White caption text on the ink.
      expect(contrast(ink, "#ffffff")).toBeGreaterThanOrEqual(4.5);
      // The pointer against a dark screen; its white outline carries it on a light one.
      expect(contrast(fill, "#000000")).toBeGreaterThanOrEqual(3);
    }
  });

  it("go by session, the first for a recording's one session", () => {
    expect(hue(1)).toEqual(hue(0));
    expect(hue(5)).toEqual(hue(1));
    expect(hue(2)).not.toEqual(hue(1));
  });
});

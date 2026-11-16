import { describe, expect, it } from "vitest";
import { contain, locate } from "./geometry";

describe("geometry", () => {
  it("fits the frame, centred, aspect kept", () => {
    expect(contain({ left: 0, top: 0, width: 1000, height: 1000 }, { width: 1600, height: 1000 })).toEqual({
      left: 0,
      top: 187.5,
      width: 1000,
      height: 625,
    });
    expect(contain({ left: 0, top: 0, width: 0, height: 10 }, { width: 1, height: 1 })).toBeNull();
  });

  it("maps a page point to the display's points", () => {
    const drawn = { left: 0, top: 187.5, width: 1000, height: 625 };
    const display = { width: 1280, height: 800 };
    expect(locate(drawn, display, 500, 500, "off")).toEqual({ x: 640, y: 400 });
    expect(locate(drawn, display, 500, 100, "off")).toBeNull();
    expect(locate(drawn, display, -50, 100, "clamp")).toEqual({ x: 0, y: 0 });
  });
});

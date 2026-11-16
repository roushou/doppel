import { describe, expect, it } from "vitest";
import { MouseButton } from "../gen/doppel/common/v1/common_pb";
import { button, Clicks, heldButton, scrollInput } from "./pointer";

describe("pointer", () => {
  it("sends the left and right buttons only", () => {
    expect([0, 1, 2, 3].map(button)).toEqual([MouseButton.LEFT, null, MouseButton.RIGHT, null]);
    expect([0, 1, 2, 3].map(heldButton)).toEqual([MouseButton.UNSPECIFIED, MouseButton.LEFT, MouseButton.RIGHT, MouseButton.LEFT]);
  });

  it("counts quick presses in one place up to a triple, then starts again", () => {
    const clicks = new Clicks();
    const at = { x: 10, y: 10 };
    const counts = [0, 200, 400, 600].map((time) => {
      const count = clicks.press({ button: MouseButton.LEFT, at, time });
      expect(clicks.release()).toBe(count);
      return count;
    });
    expect(counts).toEqual([1, 2, 3, 1]);
  });

  it("starts a new count for a late press, a far one, or another button", () => {
    const clicks = new Clicks();
    const left = MouseButton.LEFT;
    expect(clicks.press({ button: left, at: { x: 0, y: 0 }, time: 0 })).toBe(1);
    expect(clicks.press({ button: left, at: { x: 0, y: 0 }, time: 501 })).toBe(1);
    expect(clicks.press({ button: left, at: { x: 5, y: 0 }, time: 600 })).toBe(1);
    expect(clicks.press({ button: MouseButton.RIGHT, at: { x: 5, y: 0 }, time: 700 })).toBe(1);
  });

  it("gives a release with no press a single click", () => {
    expect(new Clicks().release()).toBe(1);
  });

  it("scrolls content the other way from the wheel", () => {
    const input = scrollInput({ x: 0, y: 0 }, 0, 40);
    expect(input.action.case === "scroll" && input.action.value.dy).toBe(-40);
  });
});

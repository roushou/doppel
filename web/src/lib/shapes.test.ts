import { describe, expect, it } from "vitest";
import { Place, arrow, brackets, captionSide, colour, inset, pointer, veil } from "./shapes";

describe("a film's shapes over the screen", () => {
  // A 1280 × 800 display drawn at half size, letterboxed 10 pixels down.
  const place = new Place({ left: 0, top: 10, width: 640, height: 400 }, { width: 1280, height: 800 });

  it("scale with the picture, as they do in a video", () => {
    expect(place.scale).toBe(0.5);
    expect(place.at({ x: 100, y: 200 })).toEqual({ x: 50, y: 110 });
    expect(place.length(22)).toBe(11);
    expect(place.rect({ origin: { x: 20, y: 40 }, size: { width: 100, height: 60 } } as never)).toEqual({ left: 10, top: 30, width: 50, height: 30 });
  });

  it("are coloured as 0xRRGGBB at an opacity", () => {
    expect(colour(0xff5ca8, 0.5)).toBe("rgba(255, 92, 168, 0.5)");
    expect(colour(0x22e5ff, 2)).toBe("rgba(34, 229, 255, 1)");
  });

  it("keep an outline inside its rectangle", () => {
    expect(inset({ left: 0, top: 0, width: 10, height: 10 }, 2)).toEqual({ left: 1, top: 1, width: 8, height: 8 });
  });

  it("veil the picture but for each hole", () => {
    expect(veil({ left: 0, top: 0, width: 10, height: 10 }, [{ left: 2, top: 2, width: 3, height: 3 }])).toBe("M0,0H10V10H0ZM2,2H5V5H2Z");
  });

  it("bracket a rectangle's corners, arms at most half a side", () => {
    expect(brackets({ left: 0, top: 0, width: 4, height: 10 }, 5)).toBe("M2,0H0V2M2,0H4V2M2,10H0V8M2,10H4V8");
  });

  it("end an arrow's line at its head's base", () => {
    const { end, head } = arrow({ x: 0, y: 0 }, { x: 100, y: 0 }, 14, 8.4);
    expect(end).toEqual({ x: 86, y: 0 });
    expect(head).toBe("100,0 86,8.4 86,-8.4");
  });

  it("draw the pointer from its tip", () => {
    expect(pointer({ x: 10, y: 20 }, 24 / 22).split(" ")[0]).toBe("10,20");
  });

  it("put a caption left of its pointer near the picture's right edge", () => {
    expect(captionSide(100, 1000)).toBe("right");
    expect(captionSide(700, 1000)).toBe("right");
    expect(captionSide(701, 1000)).toBe("left");
  });
});

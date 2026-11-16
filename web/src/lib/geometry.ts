// Where a screen is drawn in its element (scaled to fit, centred, aspect
// kept) and which point of the guest's display a pointer is over, as the
// native viewer's `Placement` does.

export interface Size {
  width: number;
  height: number;
}

export interface Rect extends Size {
  left: number;
  top: number;
}

/** A point of the guest's display, in its points: the coordinates input takes. */
export interface DisplayPoint {
  x: number;
  y: number;
}

/** The frame drawn in `element`, or `null` for an empty one. */
export function contain(element: Rect, frame: Size): Rect | null {
  const sizes = [element.width, element.height, frame.width, frame.height];
  if (!sizes.every((v) => Number.isFinite(v) && v > 0)) return null;
  const scale = Math.min(element.width / frame.width, element.height / frame.height);
  const width = frame.width * scale;
  const height = frame.height * scale;
  return {
    left: element.left + (element.width - width) / 2,
    top: element.top + (element.height - height) / 2,
    width,
    height,
  };
}

/** The display point under a page point; `null` off the screen, unless clamped to its edge. */
export function locate(
  drawn: Rect,
  display: Size,
  x: number,
  y: number,
  edge: "clamp" | "off",
): DisplayPoint | null {
  let fx = (x - drawn.left) / drawn.width;
  let fy = (y - drawn.top) / drawn.height;
  const inside = fx >= 0 && fx <= 1 && fy >= 0 && fy <= 1;
  switch (edge) {
    case "off":
      if (!inside) return null;
      break;
    case "clamp":
      fx = Math.min(Math.max(fx, 0), 1);
      fy = Math.min(Math.max(fy, 0), 1);
      break;
    default: {
      const never: never = edge;
      return never;
    }
  }
  return { x: fx * display.width, y: fy * display.height };
}

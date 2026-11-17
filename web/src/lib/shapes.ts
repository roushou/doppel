// A film's shapes placed over the screen: where display points land in the
// screen's box, how big a display point is there (the shapes scale with the
// picture, as they do in a video), and each shape's geometry in CSS pixels.
// `Overlay.svelte` draws what this places, as the desktop viewer's
// `shapes.rs` does; the film decides. Pure.

import type { Rect as WireRect } from "../gen/doppel/common/v1/common_pb";
import type { Rect, Size } from "./geometry";

/** A place in CSS pixels, in the screen's box. */
export interface At {
  x: number;
  y: number;
}

/** Where a display's points land in the box the picture is drawn in. */
export class Place {
  /** CSS pixels per display point. */
  readonly scale: number;

  constructor(
    /** Where the picture is drawn in the screen's box. */
    readonly drawn: Rect,
    display: Size,
  ) {
    this.scale = display.width > 0 ? drawn.width / display.width : 0;
  }

  /** A display spot; one missing is the corner. */
  at(spot: { x: number; y: number } | undefined): At {
    return { x: this.drawn.left + (spot?.x ?? 0) * this.scale, y: this.drawn.top + (spot?.y ?? 0) * this.scale };
  }

  /** A display length. */
  length(length: number): number {
    return length * this.scale;
  }

  /** A display rectangle. */
  rect(rect: WireRect | undefined): Rect {
    const corner = this.at(rect?.origin);
    return { left: corner.x, top: corner.y, width: this.length(rect?.size?.width ?? 0), height: this.length(rect?.size?.height ?? 0) };
  }
}

/** `rgb` (0xRRGGBB) at `alpha`, 0 to 1, as CSS. */
export function colour(rgb: number, alpha: number): string {
  const a = Math.max(0, Math.min(1, alpha));
  return `rgba(${(rgb >> 16) & 0xff}, ${(rgb >> 8) & 0xff}, ${rgb & 0xff}, ${a})`;
}

/** `rect` drawn inside by `width`: an outline's middle, so it stays within the rectangle as the desktop's does. */
export function inset(rect: Rect, width: number): Rect {
  const by = Math.min(width / 2, rect.width / 2, rect.height / 2);
  return { left: rect.left + by, top: rect.top + by, width: rect.width - by * 2, height: rect.height - by * 2 };
}

/** A rectangle as an SVG subpath. */
function subpath(rect: Rect): string {
  const right = rect.left + rect.width;
  const bottom = rect.top + rect.height;
  return `M${rect.left},${rect.top}H${right}V${bottom}H${rect.left}Z`;
}

/** The whole picture less each hole, filled even-odd: a veil. */
export function veil(picture: Rect, holes: readonly Rect[]): string {
  return [picture, ...holes].map(subpath).join("");
}

/** A rectangle's four corners, each an arm `arm` long, at most half a side. */
export function brackets(rect: Rect, arm: number): string {
  const a = Math.min(arm, rect.width / 2, rect.height / 2);
  const right = rect.left + rect.width;
  const bottom = rect.top + rect.height;
  return [
    `M${rect.left + a},${rect.top}H${rect.left}V${rect.top + a}`,
    `M${right - a},${rect.top}H${right}V${rect.top + a}`,
    `M${rect.left + a},${bottom}H${rect.left}V${bottom - a}`,
    `M${right - a},${bottom}H${right}V${bottom - a}`,
  ].join("");
}

/** An arrow from `from` to `to`: its line ends at the head's base, the head (`length` long, `half` wide each side) at `to`. */
export function arrow(from: At, to: At, length: number, half: number): { end: At; head: string } {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const run = Math.max(1, Math.hypot(dx, dy));
  const [ux, uy] = [dx / run, dy / run];
  const end = { x: to.x - ux * length, y: to.y - uy * length };
  return { end, head: `${to.x},${to.y} ${end.x - uy * half},${end.y + ux * half} ${end.x + uy * half},${end.y - ux * half}` };
}

/** The agent's arrow on a 24-point grid, its tip at (4, 2.5). */
const OUTLINE: readonly (readonly [number, number])[] = [
  [4.0, 2.5],
  [4.0, 19.0],
  [8.6, 14.8],
  [11.6, 21.4],
  [14.6, 20.0],
  [11.7, 13.6],
  [18.0, 13.6],
];

/** The agent's pointer, its tip at `tip`, 22 display points tall at `scale`. */
export function pointer(tip: At, scale: number): string {
  const unit = (22 / 24) * scale;
  return OUTLINE.map(([x, y]) => `${tip.x + (x - 4) * unit},${tip.y + (y - 2.5) * unit}`).join(" ");
}

/** The widest a caption gets, in CSS pixels. */
export const CAPTION_MAX = 300;
/** Where a caption starts from its pointer's tip, in display points: just past the film's badge, level with it. */
export const CAPTION_FROM = { x: 33, y: 11 } as const;

/** Which side of its pointer a caption starting at `x` goes, the picture's right edge at `edge`: left where it would be cut off. */
export function captionSide(x: number, edge: number): "right" | "left" {
  return x + CAPTION_MAX > edge ? "left" : "right";
}

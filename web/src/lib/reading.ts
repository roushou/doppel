// What a call read off the screen by text recognition, as the page draws
// it: each line where it is, and where what the call looked for was found.
// Read once from the wire here. Pure.

import type { Rect as WireRect } from "../gen/doppel/common/v1/common_pb";
import type { TextRead as WireTextRead } from "../gen/doppel/daemon/v1/daemon_pb";
import type { RecognizedText } from "../gen/doppel/vmm/v1/vmm_pb";
import type { Rect } from "./geometry";

export interface ReadLine {
  text: string;
  frame: Rect;
}

/** In display points. */
export interface TextRead {
  lines: ReadLine[];
  /** What the call looked for: a text read's query, or a text wait's text. */
  query: string | null;
  /** Where it was found: the matched words, in reading order. */
  found: Rect[];
}

/** A wire rectangle with a finite, non-negative size; `null` otherwise. */
function rect(wire: WireRect | undefined): Rect | null {
  const origin = wire?.origin;
  const size = wire?.size;
  if (origin === undefined || size === undefined) return null;
  const values = [origin.x, origin.y, size.width, size.height];
  if (!values.every(Number.isFinite) || size.width < 0 || size.height < 0) return null;
  return { left: origin.x, top: origin.y, width: size.width, height: size.height };
}

/** Recognized text's lines; a line without a readable frame is left out. */
export function readLines(text: RecognizedText): ReadLine[] {
  return text.lines.flatMap((line) => {
    const frame = rect(line.frame);
    return frame === null ? [] : [{ text: line.text, frame }];
  });
}

/** The daemon's text read; `null` for one without its text. */
export function readText(wire: WireTextRead): TextRead | null {
  if (wire.text === undefined) return null;
  return {
    lines: readLines(wire.text),
    query: wire.query ?? null,
    found: wire.found.flatMap((found) => {
      const frame = rect(found);
      return frame === null ? [] : [frame];
    }),
  };
}

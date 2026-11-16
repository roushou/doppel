// Pointer, wheel and key input as the guest is sent it, through the VMM's
// virtual devices while this page has control.

import { create } from "@bufbuild/protobuf";
import { type Input, InputSchema, Modifier, MouseButton, PointerPhase } from "../gen/doppel/common/v1/common_pb";
import type { DisplayPoint } from "./geometry";

/** The modifiers held during a pointer event. */
export function held(event: { metaKey: boolean; shiftKey: boolean; altKey: boolean; ctrlKey: boolean }): Modifier[] {
  const all: [boolean, Modifier][] = [
    [event.metaKey, Modifier.COMMAND],
    [event.shiftKey, Modifier.SHIFT],
    [event.altKey, Modifier.OPTION],
    [event.ctrlKey, Modifier.CONTROL],
  ];
  return all.filter(([down]) => down).map(([, modifier]) => modifier);
}

/** A browser button as the guest's, or `null` for one it isn't sent. */
export function button(index: number): MouseButton | null {
  switch (index) {
    case 0:
      return MouseButton.LEFT;
    case 2:
      return MouseButton.RIGHT;
    default:
      return null;
  }
}

/** The button held during a move, from `PointerEvent.buttons`. */
export function heldButton(buttons: number): MouseButton {
  if ((buttons & 1) !== 0) return MouseButton.LEFT;
  if ((buttons & 2) !== 0) return MouseButton.RIGHT;
  return MouseButton.UNSPECIFIED;
}

export function pointerInput(
  phase: PointerPhase,
  at: DisplayPoint,
  pressed: MouseButton,
  /** For a press or release, from `Clicks`; `null` for a move. */
  clickCount: 1 | 2 | 3 | null,
  modifiers: Modifier[],
): Input {
  return create(InputSchema, {
    action: {
      case: "pointer",
      value: { phase, at, button: pressed, clickCount: clickCount ?? 0, modifiers },
    },
  });
}

/** A scroll of `dy` page pixels down (as `WheelEvent.deltaY`), in display points. */
export function scrollInput(at: DisplayPoint, dx: number, dy: number): Input {
  // The guest's dy is content motion: positive towards the top.
  return create(InputSchema, { action: { case: "scroll", value: { at, dx: -dx, dy: -dy } } });
}

/** A press as the click counter sees it: when, where on the page, and with what. */
export interface Press {
  button: MouseButton;
  at: { x: number; y: number };
  /** Milliseconds, as `Event.timeStamp`. */
  time: number;
}

/**
 * Counts repeated presses into double and triple clicks, since browsers
 * give pointer presses no count of their own (`detail` is 0). A press
 * counts on from the last when it is the same button, soon after, and
 * close by; the count runs 1 to 3, then starts again.
 */
export class Clicks {
  static readonly INTERVAL_MS = 500;
  static readonly SLOP_PX = 4;
  #last: (Press & { count: 1 | 2 | 3 }) | null = null;

  /** The count of a press, which the release that ends it repeats. */
  press(press: Press): 1 | 2 | 3 {
    const last = this.#last;
    const again =
      last !== null &&
      last.button === press.button &&
      press.time - last.time <= Clicks.INTERVAL_MS &&
      Math.hypot(press.at.x - last.at.x, press.at.y - last.at.y) <= Clicks.SLOP_PX;
    const count = again && last.count !== 3 ? ((last.count + 1) as 2 | 3) : 1;
    this.#last = { ...press, count };
    return count;
  }

  /** The count of the press a release ends. */
  release(): 1 | 2 | 3 {
    return this.#last?.count ?? 1;
  }
}

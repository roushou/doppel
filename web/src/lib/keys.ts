// A browser key press as the guest is sent it: a key and the modifiers
// held, as the native viewer sends them. Characters typed plainly (or with
// Shift) go as the character itself; with Command, Control or Option held,
// as the key's base character and those modifiers, so Command-Shift-T is
// "t" with Command and Shift whatever the layout makes of it.

import { create } from "@bufbuild/protobuf";
import { type Input, InputSchema, Modifier, NamedKey } from "../gen/doppel/common/v1/common_pb";

/** The parts of a `KeyboardEvent` this reads. */
export interface Press {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

const NAMED: Record<string, NamedKey> = {
  Enter: NamedKey.RETURN,
  Tab: NamedKey.TAB,
  " ": NamedKey.SPACE,
  Backspace: NamedKey.DELETE,
  Delete: NamedKey.FORWARD_DELETE,
  Escape: NamedKey.ESCAPE,
  ArrowLeft: NamedKey.LEFT,
  ArrowRight: NamedKey.RIGHT,
  ArrowUp: NamedKey.UP,
  ArrowDown: NamedKey.DOWN,
  Home: NamedKey.HOME,
  End: NamedKey.END,
  PageUp: NamedKey.PAGE_UP,
  PageDown: NamedKey.PAGE_DOWN,
  F1: NamedKey.F1,
  F2: NamedKey.F2,
  F3: NamedKey.F3,
  F4: NamedKey.F4,
  F5: NamedKey.F5,
  F6: NamedKey.F6,
  F7: NamedKey.F7,
  F8: NamedKey.F8,
  F9: NamedKey.F9,
  F10: NamedKey.F10,
  F11: NamedKey.F11,
  F12: NamedKey.F12,
};

/** The US-layout character of a physical key, for chords. */
const BASE: Record<string, string> = {
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
};

function base(press: Press): string | null {
  const letter = /^Key([A-Z])$/.exec(press.code);
  if (letter?.[1] !== undefined) return letter[1].toLowerCase();
  const digit = /^Digit([0-9])$/.exec(press.code);
  if (digit?.[1] !== undefined) return digit[1];
  const known = BASE[press.code];
  if (known !== undefined) return known;
  return [...press.key].length === 1 ? press.key.toLowerCase() : null;
}

function modifiers(press: Press, withShift: boolean): Modifier[] {
  const held: [boolean, Modifier][] = [
    [press.metaKey, Modifier.COMMAND],
    [withShift && press.shiftKey, Modifier.SHIFT],
    [press.altKey, Modifier.OPTION],
    [press.ctrlKey, Modifier.CONTROL],
  ];
  return held.filter(([down]) => down).map(([, modifier]) => modifier);
}

type KeyOf = { case: "named"; value: NamedKey } | { case: "character"; value: string };

function chord(key: KeyOf, held: Modifier[]): Input {
  return create(InputSchema, { action: { case: "key", value: { modifiers: held, key: { key } } } });
}

/** What the guest is sent for a press, or `null` for one it isn't: a lone modifier, a dead key, a key mid-composition. */
export function keyInput(press: Press): Input | null {
  const named = NAMED[press.key];
  if (named !== undefined) return chord({ case: "named", value: named }, modifiers(press, true));
  const chorded = press.metaKey || press.ctrlKey || press.altKey;
  if (chorded) {
    const character = base(press);
    return character === null ? null : chord({ case: "character", value: character }, modifiers(press, true));
  }
  // One grapheme typed as it is; Shift is already in it.
  if ([...press.key].length !== 1) return null;
  return chord({ case: "character", value: press.key }, []);
}

/**
 * What the guest is sent for an edit a text field was about to make, as
 * `InputEvent.inputType` and `data` describe it: how an on-screen
 * keyboard types, since it sends no key presses a page can read. Text
 * goes a character at a time; an edit that isn't typing (a paste, an
 * undo, a deletion by word, a composition, whose text comes when it
 * ends) sends nothing.
 */
export function editInput(inputType: string, data: string | null): Input[] {
  switch (inputType) {
    case "insertText":
    case "insertReplacementText":
      return [...(data ?? "")].map((character) =>
        character === "\n"
          ? chord({ case: "named", value: NamedKey.RETURN }, [])
          : chord({ case: "character", value: character }, []),
      );
    case "insertLineBreak":
    case "insertParagraph":
      return [chord({ case: "named", value: NamedKey.RETURN }, [])];
    case "deleteContentBackward":
      return [chord({ case: "named", value: NamedKey.DELETE }, [])];
    case "deleteContentForward":
      return [chord({ case: "named", value: NamedKey.FORWARD_DELETE }, [])];
    default:
      return [];
  }
}

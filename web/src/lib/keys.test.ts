import { describe, expect, it } from "vitest";
import { Modifier, NamedKey } from "../gen/doppel/common/v1/common_pb";
import { editInput, keyInput, type Press } from "./keys";

const press = (key: string, code: string, held: Partial<Press> = {}): Press => ({
  key,
  code,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...held,
});

function sent(p: Press) {
  const input = keyInput(p);
  if (input === null) return null;
  if (input.action.case !== "key") throw new Error("not a key");
  return { key: input.action.value.key?.key, modifiers: input.action.value.modifiers };
}

describe("keys", () => {
  it("send typed characters as they are, Shift included", () => {
    expect(sent(press("a", "KeyA"))).toEqual({ key: { case: "character", value: "a" }, modifiers: [] });
    expect(sent(press("A", "KeyA", { shiftKey: true }))).toEqual({ key: { case: "character", value: "A" }, modifiers: [] });
    expect(sent(press("é", "KeyE"))).toEqual({ key: { case: "character", value: "é" }, modifiers: [] });
  });

  it("send chords as the key's base character and the modifiers", () => {
    expect(sent(press("T", "KeyT", { metaKey: true, shiftKey: true }))).toEqual({
      key: { case: "character", value: "t" },
      modifiers: [Modifier.COMMAND, Modifier.SHIFT],
    });
    // Option's character is the layout's; the guest gets the key.
    expect(sent(press("å", "KeyA", { altKey: true }))).toEqual({
      key: { case: "character", value: "a" },
      modifiers: [Modifier.OPTION],
    });
    expect(sent(press("/", "Slash", { ctrlKey: true }))?.key).toEqual({ case: "character", value: "/" });
  });

  it("send named keys with every modifier held", () => {
    expect(sent(press("Enter", "Enter"))).toEqual({ key: { case: "named", value: NamedKey.RETURN }, modifiers: [] });
    expect(sent(press("ArrowLeft", "ArrowLeft", { shiftKey: true, altKey: true }))).toEqual({
      key: { case: "named", value: NamedKey.LEFT },
      modifiers: [Modifier.SHIFT, Modifier.OPTION],
    });
    expect(sent(press(" ", "Space"))?.key).toEqual({ case: "named", value: NamedKey.SPACE });
  });

  it("send nothing for what isn't a key of its own", () => {
    for (const p of [press("Shift", "ShiftLeft", { shiftKey: true }), press("Meta", "MetaLeft"), press("Dead", "Quote"), press("Process", "KeyA"), press("CapsLock", "CapsLock")]) {
      expect(keyInput(p)).toBeNull();
    }
  });
});

describe("edits from an on-screen keyboard", () => {
  const keys = (inputType: string, data: string | null) =>
    editInput(inputType, data).map((input) => (input.action.case === "key" ? input.action.value.key?.key : null));

  it("types text a character at a time", () => {
    expect(keys("insertText", "hé")).toEqual([
      { case: "character", value: "h" },
      { case: "character", value: "é" },
    ]);
  });

  it("sends Return and the deletions as keys", () => {
    expect(keys("insertLineBreak", null)).toEqual([{ case: "named", value: NamedKey.RETURN }]);
    expect(keys("insertText", "\n")).toEqual([{ case: "named", value: NamedKey.RETURN }]);
    expect(keys("deleteContentBackward", null)).toEqual([{ case: "named", value: NamedKey.DELETE }]);
    expect(keys("deleteContentForward", null)).toEqual([{ case: "named", value: NamedKey.FORWARD_DELETE }]);
  });

  it("sends nothing for an edit that isn't typing", () => {
    expect(editInput("insertFromPaste", "secret")).toEqual([]);
    expect(editInput("historyUndo", null)).toEqual([]);
    expect(editInput("deleteWordBackward", null)).toEqual([]);
    expect(editInput("insertCompositionText", "に")).toEqual([]);
  });
});

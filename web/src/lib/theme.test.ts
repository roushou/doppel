import { describe, expect, it } from "vitest";
import { nextTheme, readTheme, shownTheme } from "./theme";

describe("the theme switch", () => {
  it("cycles system, light, dark and back", () => {
    expect(nextTheme("system")).toBe("light");
    expect(nextTheme("light")).toBe("dark");
    expect(nextTheme("dark")).toBe("system");
  });

  it("reads a kept choice, the system's when unrecognized", () => {
    expect(readTheme("dark")).toBe("dark");
    expect(readTheme("light")).toBe("light");
    expect(readTheme(null)).toBe("system");
    expect(readTheme("sepia")).toBe("system");
  });

  it("follows the system only when told to", () => {
    expect(shownTheme("system", true)).toBe("light");
    expect(shownTheme("system", false)).toBe("dark");
    expect(shownTheme("light", false)).toBe("light");
    expect(shownTheme("dark", true)).toBe("dark");
  });
});

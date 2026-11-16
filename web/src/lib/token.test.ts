import { describe, expect, it } from "vitest";
import { parseToken, tokenCookie, tokenFromFragment } from "./token";

const TOKEN = `dpl_${"A".repeat(43)}`;

describe("tokens", () => {
  it("come from the fragment, which is left without them", () => {
    expect(tokenFromFragment(`#token=${TOKEN}`)).toEqual({ token: TOKEN, rest: "" });
    expect(tokenFromFragment(`#a=1&token=${TOKEN}`)).toEqual({ token: TOKEN, rest: "#a=1" });
    expect(tokenFromFragment("")).toEqual({ token: null, rest: "" });
  });

  it("have one shape", () => {
    expect(parseToken(TOKEN)).toBe(TOKEN);
    for (const wrong of ["", "dpl_", `dpl_${"A".repeat(42)}`, `dpl_${"A".repeat(44)}`, `DPL_${"A".repeat(43)}`, `dpl_${"+".repeat(43)}`]) {
      expect(parseToken(wrong)).toBeNull();
    }
  });

  it("go to recordings' videos in a cookie that only they get", () => {
    const token = parseToken(`dpl_${"a".repeat(43)}`);
    expect(tokenCookie(token)).toBe(`doppel-token=dpl_${"a".repeat(43)}; Path=/recordings/; Secure; SameSite=Strict`);
    expect(tokenCookie(null)).toBe("doppel-token=; Max-Age=0; Path=/recordings/; Secure; SameSite=Strict");
  });
});

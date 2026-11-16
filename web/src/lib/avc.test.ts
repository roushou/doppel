import { describe, expect, it } from "vitest";
import { avcC, codecString } from "./avc";

// A High-profile level 4.2 SPS's first bytes, and a PPS.
const SPS = new Uint8Array([0x67, 0x64, 0x00, 0x2a, 0xac, 0x2b]);
const PPS = new Uint8Array([0x68, 0xee, 0x3c, 0xb0]);

describe("avc", () => {
  it("names the codec by profile, constraints and level", () => {
    expect(codecString(SPS)).toBe("avc1.64002a");
  });

  it("records one SPS and one PPS with 4-byte lengths", () => {
    expect(Array.from(avcC(SPS, PPS))).toEqual([
      1, 0x64, 0x00, 0x2a, 0xff, 0xe1, 0, 6, ...SPS, 1, 0, 4, ...PPS,
    ]);
  });
});

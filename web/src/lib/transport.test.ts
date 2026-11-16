import { describe, expect, it } from "vitest";
import { follow, Transport } from "./transport";

describe("Transport", () => {
  it("starts paused at 0", () => {
    const t = Transport.start(5000);
    expect(Transport.at(t, 123)).toBe(0);
  });

  it("plays on the wall clock and pauses where it got to", () => {
    let t = Transport.play(Transport.start(5000), 1000);
    expect(Transport.at(t, 1500)).toBe(500);
    t = Transport.pause(t, 1800);
    expect(t).toEqual({ kind: "paused", at: 800, length: 5000 });
    expect(Transport.at(t, 9999)).toBe(800);
  });

  it("plays past a video's end, up to the session's length, then ends", () => {
    const t = Transport.play(Transport.start(5000), 0);
    expect(Transport.at(t, 7000)).toBe(5000);
    expect(Transport.settle(t, 4999).kind).toBe("playing");
    expect(Transport.settle(t, 5000)).toEqual({ kind: "ended", length: 5000 });
  });

  it("plays from the start again once ended", () => {
    const ended = Transport.settle(Transport.play(Transport.start(100), 0), 200);
    expect(Transport.at(Transport.play(ended, 300), 350)).toBe(50);
  });

  it("seeks within the session, keeping play or pause", () => {
    const playing = Transport.seek(Transport.play(Transport.start(5000), 0), 3000, 100);
    expect(Transport.at(playing, 600)).toBe(3500);
    const paused = Transport.seek(Transport.start(5000), 9000, 0);
    expect(paused).toEqual({ kind: "paused", at: 5000, length: 5000 });
    expect(Transport.at(Transport.seek(paused, -5, 0), 0)).toBe(0);
  });

  it("lengthens, but never shortens; an ended clock pauses at its old end", () => {
    const t = Transport.start(1000);
    expect(Transport.lengthen(t, 500, 0)).toBe(t);
    const ended = Transport.settle(Transport.play(t, 0), 2000);
    expect(Transport.lengthen(ended, 3000, 0)).toEqual({ kind: "paused", at: 1000, length: 3000 });
  });
});

describe("follow", () => {
  it("is inside the video before its end, or while its length isn't known", () => {
    expect(follow(2, 10)).toEqual({ kind: "inside", seconds: 2 });
    expect(follow(20, Number.NaN)).toEqual({ kind: "inside", seconds: 20 });
  });

  it("holds the last frame past the video's end", () => {
    expect(follow(12, 10)).toEqual({ kind: "after", seconds: 10 });
  });
});

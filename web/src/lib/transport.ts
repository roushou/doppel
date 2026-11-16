// A replay's clock: the session's time, played or paused, from 0 to the
// session's length. It runs on its own, not on a video's: a recording's
// video ends at the screen's last change, which may be long before the
// trace does, and the trace must still play and seek to its end. The video
// follows the clock. Pure: the wall clock is passed in.

/** Where the clock stands. Every time is in ms; `at` is the session's time. */
export type Transport =
  | { kind: "paused"; at: number; length: number }
  | { kind: "playing"; from: number; since: number; length: number }
  | { kind: "ended"; length: number };

export const Transport = {
  /** Paused at the start of a session `length` ms long. */
  start(length: number): Transport {
    return { kind: "paused", at: 0, length: Math.max(0, length) };
  },

  /** The session's time at the wall clock's `wall`. */
  at(transport: Transport, wall: number): number {
    switch (transport.kind) {
      case "paused":
        return transport.at;
      case "playing":
        return Math.min(transport.length, transport.from + Math.max(0, wall - transport.since));
      case "ended":
        return transport.length;
      default: {
        const never: never = transport;
        return never;
      }
    }
  },

  /** Plays from where it is; from the start again once it ended. */
  play(transport: Transport, wall: number): Transport {
    switch (transport.kind) {
      case "paused":
        return transport.at >= transport.length
          ? { kind: "playing", from: 0, since: wall, length: transport.length }
          : { kind: "playing", from: transport.at, since: wall, length: transport.length };
      case "playing":
        return transport;
      case "ended":
        return { kind: "playing", from: 0, since: wall, length: transport.length };
      default: {
        const never: never = transport;
        return never;
      }
    }
  },

  /** Stops where it is. */
  pause(transport: Transport, wall: number): Transport {
    switch (transport.kind) {
      case "paused":
      case "ended":
        return transport;
      case "playing":
        return { kind: "paused", at: Transport.at(transport, wall), length: transport.length };
      default: {
        const never: never = transport;
        return never;
      }
    }
  },

  /** Moves to the session's time `ms`, clamped to the session; playing on if it was. */
  seek(transport: Transport, ms: number, wall: number): Transport {
    const at = Math.min(transport.length, Math.max(0, ms));
    switch (transport.kind) {
      case "playing":
        return { kind: "playing", from: at, since: wall, length: transport.length };
      case "paused":
      case "ended":
        return { kind: "paused", at, length: transport.length };
      default: {
        const never: never = transport;
        return never;
      }
    }
  },

  /** The clock as of `wall`: a playing clock that reached the end has ended. */
  settle(transport: Transport, wall: number): Transport {
    return transport.kind === "playing" && Transport.at(transport, wall) >= transport.length
      ? { kind: "ended", length: transport.length }
      : transport;
  },

  /** A longer session, as more of it becomes known (a video's length, once loaded). */
  lengthen(transport: Transport, length: number, wall: number): Transport {
    if (length <= transport.length) return transport;
    switch (transport.kind) {
      case "paused":
      case "playing":
        return { ...transport, length };
      case "ended":
        return { kind: "paused", at: Transport.at(transport, wall), length };
      default: {
        const never: never = transport;
        return never;
      }
    }
  },
};

/** What a video should do for the clock to show the session's time. */
export type Follow =
  /** Seconds into the video, inside it. */
  | { kind: "inside"; seconds: number }
  /** Past its end: its last frame, the screen as it stayed. */
  | { kind: "after"; seconds: number };

/**
 * Where a video `duration` seconds long (NaN until known) should be for
 * the session's time `seconds` into it.
 */
export function follow(seconds: number, duration: number): Follow {
  if (Number.isFinite(duration) && seconds >= duration) return { kind: "after", seconds: duration };
  return { kind: "inside", seconds: Math.max(0, seconds) };
}

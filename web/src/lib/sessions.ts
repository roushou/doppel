// The daemon's session list, parsed once into what the page shows: who
// drives which machines, what they're doing, and whether they're held.

import { type SessionList, SessionState } from "../gen/doppel/daemon/v1/daemon_pb";
import type { MachineName } from "./machines";

declare const brand: unique symbol;

/** A session's id, as the daemon numbers them. */
export type SessionId = bigint & { readonly [brand]: "SessionId" };

/** Whether a session's calls run; a state a newer daemon added keeps its number. */
export type Activity = { kind: "active" } | { kind: "paused" } | { kind: "unknown"; raw: number };

export interface Session {
  id: SessionId;
  /** Who drives, in its own words. */
  client: string;
  /** The machines its requests named, in first-use order. */
  machines: MachineName[];
  /** Requests answered so far. */
  answered: bigint;
  /** Captions of the requests running or waiting now. */
  running: string[];
  activity: Activity;
}

function activity(state: SessionState): Activity {
  switch (state) {
    case SessionState.ACTIVE:
      return { kind: "active" };
    case SessionState.PAUSED:
      return { kind: "paused" };
    case SessionState.UNSPECIFIED:
      return { kind: "unknown", raw: state };
    default:
      // A state a newer daemon added.
      return { kind: "unknown", raw: state };
  }
}

export function parseSessions(list: SessionList): Session[] {
  return list.sessions.map((entry) => ({
    id: entry.id as SessionId,
    client: entry.client,
    machines: entry.machines.map((name) => name as MachineName),
    answered: entry.answered,
    running: [...entry.running],
    activity: activity(entry.state),
  }));
}

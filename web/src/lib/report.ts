// A machine's report, parsed once into what the status bar shows: whether
// its guest agent answers, the app in front, the display's size, and how
// many checkpoints it keeps.

import { SessionState } from "../gen/doppel/agent/v1/agent_pb";
import type { MachineReport } from "../gen/doppel/daemon/v1/daemon_pb";

/** The guest agent, as the daemon last found it. */
export type Guest =
  | {
      kind: "answering";
      /** Whether someone is logged in and the screen unlocked. */
      login: "active" | "locked" | "unknown";
      /** The app in front, by name; `null` when the agent doesn't say. */
      front: string | null;
      /** The display, in points; `null` when the agent doesn't say. */
      display: { width: number; height: number } | null;
    }
  | { kind: "unreachable"; why: string }
  | { kind: "not-running" }
  | { kind: "unknown" };

export interface Report {
  guest: Guest;
  checkpoints: number;
}

function login(state: SessionState): "active" | "locked" | "unknown" {
  switch (state) {
    case SessionState.ACTIVE:
      return "active";
    case SessionState.LOCKED:
      return "locked";
    case SessionState.UNSPECIFIED:
      return "unknown";
    default:
      // A state a newer agent added.
      return "unknown";
  }
}

export function parseReport(report: MachineReport): Report {
  const checkpoints = report.checkpoints.length;
  switch (report.agent.case) {
    case "connected": {
      const status = report.agent.value;
      const size = status.displayPoints;
      const front = status.frontmostApp?.name ?? "";
      return {
        checkpoints,
        guest: {
          kind: "answering",
          login: login(status.session),
          front: front === "" ? null : front,
          display: size === undefined || size.width <= 0 || size.height <= 0 ? null : { width: size.width, height: size.height },
        },
      };
    }
    case "unreachable":
      return { checkpoints, guest: { kind: "unreachable", why: report.agent.value } };
    case "notRunning":
      return { checkpoints, guest: { kind: "not-running" } };
    case undefined:
      return { checkpoints, guest: { kind: "unknown" } };
    default: {
      const never: never = report.agent;
      return never;
    }
  }
}

/** Calls an agent made on a machine in the last minute, from its timeline. */
export function callsPerMinute(starts: readonly number[], now: number): number {
  return starts.filter((at) => at <= now && now - at < 60_000).length;
}

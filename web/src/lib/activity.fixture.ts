// Activity events as the daemon sends them, read by the page, for tests.

import { create } from "@bufbuild/protobuf";
import { ActivitySchema, Gesture, MarkShape } from "../gen/doppel/daemon/v1/daemon_pb";
import { parseActivity } from "./overlay";

export function started(request: number, gesture: Gesture, target: object) {
  const event = parseActivity(
    create(ActivitySchema, {
      session: 1n,
      request: BigInt(request),
      machine: "dev",
      client: "agent",
      caption: "a call",
      phase: { case: "started", value: { gesture, target: target as never } },
    }),
  );
  if (event === null) throw new Error("unread");
  return event;
}

/** A mark call's start: a dot with a note on a point, lasting `lasts`. */
export function marked(request: number, lasts: { case: "forMs" | "untilActedMs"; value: number }) {
  const event = parseActivity(
    create(ActivitySchema, {
      session: 1n,
      request: BigInt(request),
      machine: "dev",
      client: "agent",
      caption: "mark",
      phase: {
        case: "started",
        value: {
          gesture: Gesture.MARK,
          mark: { shape: MarkShape.DOT, at: { case: "point", value: { x: 40, y: 50 } }, note: " here ", lasts },
        },
      },
    }),
  );
  if (event === null) throw new Error("unread");
  return event;
}

export function ended(request: number, outcome: object, seen?: object, read?: object) {
  const event = parseActivity(
    create(ActivitySchema, {
      session: 1n,
      request: BigInt(request),
      machine: "dev",
      phase: { case: "ended", value: { outcome: outcome as never, seen: seen as never, read: read as never } },
    }),
  );
  if (event === null) throw new Error("unread");
  return event;
}

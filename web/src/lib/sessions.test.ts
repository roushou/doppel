import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionListSchema, SessionState } from "../gen/doppel/daemon/v1/daemon_pb";
import { parseSessions } from "./sessions";

describe("sessions", () => {
  it("reads each session's driver, machines, work and state", () => {
    const list = create(SessionListSchema, {
      sessions: [
        { id: 3n, client: "claude", machines: ["live"], answered: 12n, running: ["click OK"], state: SessionState.PAUSED },
        { id: 4n, client: "doppel mcp", machines: [], answered: 0n, running: [], state: SessionState.ACTIVE },
      ],
    });
    expect(parseSessions(list)).toEqual([
      { id: 3n, client: "claude", machines: ["live"], answered: 12n, running: ["click OK"], activity: { kind: "paused" } },
      { id: 4n, client: "doppel mcp", machines: [], answered: 0n, running: [], activity: { kind: "active" } },
    ]);
  });

  it("keeps a state it doesn't know, with its number", () => {
    const list = create(SessionListSchema, { sessions: [{ id: 1n, state: 7 as SessionState }] });
    expect(parseSessions(list)[0]?.activity).toEqual({ kind: "unknown", raw: 7 });
  });
});

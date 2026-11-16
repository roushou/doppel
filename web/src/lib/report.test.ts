import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionState } from "../gen/doppel/agent/v1/agent_pb";
import { DoneSchema, MachineReportSchema } from "../gen/doppel/daemon/v1/daemon_pb";
import { callsPerMinute, parseReport } from "./report";

describe("a machine's report", () => {
  it("reads the guest agent's login, front app and display", () => {
    const report = create(MachineReportSchema, {
      checkpoints: ["clean", "calc-open"],
      agent: {
        case: "connected",
        value: { session: SessionState.ACTIVE, displayPoints: { width: 1440, height: 900 }, frontmostApp: { pid: 4, name: "Calculator" } },
      },
    });
    expect(parseReport(report)).toEqual({
      checkpoints: 2,
      guest: { kind: "answering", login: "active", front: "Calculator", display: { width: 1440, height: 900 } },
    });
  });

  it("leaves out what the agent doesn't say", () => {
    const report = create(MachineReportSchema, { agent: { case: "connected", value: { session: 9 as SessionState } } });
    expect(parseReport(report).guest).toEqual({ kind: "answering", login: "unknown", front: null, display: null });
  });

  it("says why the agent can't be asked", () => {
    expect(parseReport(create(MachineReportSchema, { agent: { case: "unreachable", value: "timed out" } })).guest).toEqual({
      kind: "unreachable",
      why: "timed out",
    });
    expect(parseReport(create(MachineReportSchema, { agent: { case: "notRunning", value: create(DoneSchema) } })).guest).toEqual({
      kind: "not-running",
    });
    expect(parseReport(create(MachineReportSchema)).guest).toEqual({ kind: "unknown" });
  });
});

describe("calls per minute", () => {
  it("counts the calls that started in the last minute", () => {
    expect(callsPerMinute([0, 30_000, 59_999, 61_000, 90_000], 90_000)).toBe(3);
    expect(callsPerMinute([], 5)).toBe(0);
  });
});

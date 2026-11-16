import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  CheckpointReportSchema,
  Certainty,
  ForksRunSchema,
  MachineFailureSchema,
  MachineStoppedSchema,
  StopOutcome,
} from "../gen/doppel/daemon/v1/daemon_pb";
import { StopMode } from "../gen/doppel/vmm/v1/vmm_pb";
import { checkpointLabel, machineName } from "./lifecycle";
import type { MachineName } from "./machines";
import { operation, verdict } from "./operate";

const dev = "dev" as MachineName;

function label(raw: string) {
  const parsed = checkpointLabel(raw);
  if (parsed === null) throw new Error(`bad label ${raw}`);
  return parsed;
}

describe("operation", () => {
  it("stops gracefully unless forced", () => {
    const graceful = operation(dev, { action: "stop" });
    const forced = operation(dev, { action: "force-stop" });
    expect(graceful.case === "stopMachine" && graceful.value.mode).toBe(StopMode.GRACEFUL);
    expect(forced.case === "stopMachine" && forced.value.mode).toBe(StopMode.FORCE);
  });

  it("makes a new copy as one fresh fork", () => {
    const copy = operation(dev, { action: "new-copy", label: label("base") });
    expect(copy.case).toBe("runForks");
    expect(copy.case === "runForks" && [copy.value.name, copy.value.label, copy.value.count]).toEqual(["dev", "base", 1]);
  });

  it("forks a machine's checkpoint into the name given", () => {
    const into = machineName("dev-2");
    if (into === null) throw new Error("bad name");
    const fork = operation(dev, { action: "fork", label: label("base"), into });
    expect(fork.case === "forkMachine" && [fork.value.source, fork.value.label, fork.value.into]).toEqual([
      "dev",
      "base",
      "dev-2",
    ]);
  });
});

describe("verdict", () => {
  it("tells a refusal from a failure that may have happened", () => {
    const failure = (certainty: Certainty) =>
      verdict({ case: "machineFailure", value: create(MachineFailureSchema, { message: "x", certainty }) }).kind;
    expect(failure(Certainty.NOT_DONE)).toBe("refused");
    expect(failure(Certainty.MAYBE_DONE)).toBe("unknown");
    expect(failure(Certainty.UNSPECIFIED)).toBe("unknown");
  });

  it("never reads a checkpoint of unknown outcome as saved", () => {
    const unknown = create(CheckpointReportSchema, { result: { case: "unknown", value: {} } });
    expect(verdict({ case: "checkpointReport", value: unknown }).kind).toBe("unknown");
    const unfinished = create(CheckpointReportSchema, { result: { case: "inProgress", value: 0 } });
    expect(verdict({ case: "checkpointReport", value: unfinished }).kind).toBe("unknown");
  });

  it("says when a stop is still pending", () => {
    const pending = create(MachineStoppedSchema, { outcome: StopOutcome.SHUTDOWN_PENDING });
    const read = verdict({ case: "machineStopped", value: pending });
    expect(read.kind).toBe("done");
    expect(read.kind === "done" && read.note).not.toBeNull();
  });

  it("refuses a copy that failed to start", () => {
    const run = create(ForksRunSchema, {
      forks: [{ name: "dev-base-1", outcome: { case: "failure", value: { message: "no" } } }],
    });
    expect(verdict({ case: "forksRun", value: run }).kind).toBe("refused");
  });
});

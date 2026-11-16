import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { MachineListSchema, NextStart } from "../gen/doppel/daemon/v1/daemon_pb";
import { VmRunState } from "../gen/doppel/vmm/v1/vmm_pb";
import { machineActions, preselected, shown } from "./lifecycle";
import { forksOf, group, type Machine, type MachineName, parseMachines, provenance, template, UNRECORDED } from "./machines";
import type { Label } from "./operate";

const stoppedStatus = { case: "machineStatus", value: { kind: { case: "stopped", value: NextStart.COLD_BOOT } } } as const;
const label = (raw: string) => raw as Label;

describe("machines", () => {
  it("are live, stopped, or unreadable", () => {
    const list = create(MachineListSchema, {
      machines: [
        { name: "dev", status: { case: "machineStatus", value: { kind: { case: "live", value: { runState: VmRunState.RUNNING } } } } },
        { name: "golden", status: stoppedStatus },
        { name: "odd", status: { case: "failure", value: { message: "bad spec" } } },
        { name: "new", status: { case: "machineStatus", value: { kind: { case: "live", value: { runState: 99 as VmRunState } } } } },
      ],
    });
    expect(parseMachines(list)).toEqual([
      { name: "dev", state: { kind: "live", run: "running" }, lineage: UNRECORDED },
      { name: "golden", state: { kind: "stopped" }, lineage: UNRECORDED },
      { name: "odd", state: { kind: "unreadable", reason: "bad spec" }, lineage: UNRECORDED },
      { name: "new", state: { kind: "live", run: "unknown" }, lineage: UNRECORDED },
    ]);
  });

  it("carry their role and origin, and an unreadable lineage is never a default", () => {
    const list = create(MachineListSchema, {
      machines: [
        {
          name: "golden-27",
          status: stoppedStatus,
          recordedLineage: {
            case: "lineage",
            value: { role: { case: "golden", value: { checkpoint: "ready" } }, origin: { case: "installed", value: {} } },
          },
        },
        {
          name: "copy",
          status: stoppedStatus,
          recordedLineage: {
            case: "lineage",
            value: {
              role: { case: "working", value: {} },
              origin: { case: "forked", value: { machine: "golden-27", checkpoint: "ready" } },
            },
          },
        },
        { name: "broken", status: stoppedStatus, recordedLineage: { case: "lineageUnreadable", value: "truncated" } },
        // A newer daemon's role: neither golden nor working.
        { name: "newer", status: stoppedStatus, recordedLineage: { case: "lineage", value: { origin: { case: "imported", value: {} } } } },
        {
          name: "bad",
          status: stoppedStatus,
          recordedLineage: {
            case: "lineage",
            value: { role: { case: "golden", value: { checkpoint: "Not A Label" } }, origin: { case: "imported", value: {} } },
          },
        },
      ],
    });
    const parsed = parseMachines(list);
    const at = (index: number): Machine => {
      const machine = parsed[index];
      if (machine === undefined) throw new Error(`no machine ${index}`);
      return machine;
    };
    const [golden, copy, broken, newer, bad] = [0, 1, 2, 3, 4].map(at) as [Machine, Machine, Machine, Machine, Machine];
    expect(golden.lineage).toEqual({ kind: "known", role: { kind: "golden", template: "ready" }, origin: { kind: "installed" } });
    expect(copy.lineage).toEqual({
      kind: "known",
      role: { kind: "working" },
      origin: { kind: "forked", from: "golden-27", checkpoint: "ready" },
    });
    expect(broken.lineage).toEqual({ kind: "unreadable", reason: "truncated" });
    expect(newer.lineage.kind).toBe("unreadable");
    expect(bad.lineage.kind).toBe("unreadable");

    expect(template(golden)).toBe("ready");
    expect([golden, copy, broken].map(group)).toEqual(["golden", "stopped", "stopped"]);
    expect(provenance(golden)).toEqual({ kind: "said", text: "Template · ready" });
    expect(provenance(copy)).toEqual({ kind: "said", text: "From golden-27 · ready" });
    expect(provenance(broken)).toEqual({ kind: "unreadable", reason: "truncated" });
    expect(provenance({ ...copy, lineage: UNRECORDED })).toBeNull();
    expect(forksOf([golden, copy, broken], golden.name)).toEqual([copy.name]);
    expect(forksOf([golden, copy, broken], copy.name)).toEqual([]);
  });

  it("golden images are listed apart whatever their state, and make machines first", () => {
    const golden: Machine = {
      name: "golden-27" as MachineName,
      state: { kind: "stopped" },
      lineage: { kind: "known", role: { kind: "golden", template: label("ready") }, origin: { kind: "installed" } },
    };
    const running: Machine = { ...golden, state: { kind: "live", run: "running" } };
    expect(group(running)).toBe("golden");
    const actions = machineActions(golden);
    expect(actions[0]).toBe("from-template");
    expect(shown("from-template")).toBe(true);
    expect(actions).not.toContain("start");
    expect(actions).toContain("start-golden");
    expect(shown("start-golden")).toBe(false);
    // Running anyway, it's handled as any running machine.
    expect(machineActions(running)).toContain("pause");
    expect(machineActions({ ...golden, lineage: UNRECORDED })).toContain("start");
  });

  it("a new machine from a golden image preselects its template once read", () => {
    const labels = [label("ready"), label("later")];
    expect(preselected(labels, label("ready"))).toBe("ready");
    expect(preselected(labels, label("gone"))).toBe("later");
    expect(preselected(labels, null)).toBe("later");
    expect(preselected([], null)).toBeNull();
  });
});

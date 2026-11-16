// A lifecycle action as the daemon request that does it, and the daemon's
// answer as what the page tells the person. Pure: no connection here.

import { create } from "@bufbuild/protobuf";
import {
  AfterCheckpoint,
  Certainty,
  DeleteMachineSchema,
  ForkMachineSchema,
  PauseMachineSchema,
  RestoreMachineSchema,
  ResumeMachineSchema,
  RollBackSchema,
  RunForksSchema,
  StartMachineSchema,
  StopMachineSchema,
  StopOutcome,
  TakeCheckpointSchema,
  Window,
} from "../gen/doppel/daemon/v1/daemon_pb";
import { StopMode } from "../gen/doppel/vmm/v1/vmm_pb";
import type { Operation, Outcome } from "./connection";
import { refusal } from "./connection";
import type { MachineName } from "./machines";

declare const brand: unique symbol;

/** A checkpoint label, already checked by `checkpointLabel`. */
export type Label = string & { readonly [brand]: "Label" };

/** What an action does, with what it needs. */
export type Order =
  | { action: "start" }
  | { action: "stop" }
  | { action: "force-stop" }
  | { action: "pause" }
  | { action: "resume" }
  | { action: "checkpoint"; label: Label }
  | { action: "roll-back"; label: Label }
  | { action: "restore"; label: Label }
  | { action: "fork"; label: Label; into: MachineName }
  | { action: "new-copy"; label: Label }
  | { action: "delete" };

/** The daemon request that carries out `order` on `name`. */
export function operation(name: MachineName, order: Order): Operation {
  switch (order.action) {
    case "start":
      return { case: "startMachine", value: create(StartMachineSchema, { name, window: Window.HIDDEN }) };
    case "stop":
      return { case: "stopMachine", value: create(StopMachineSchema, { name, mode: StopMode.GRACEFUL }) };
    case "force-stop":
      return { case: "stopMachine", value: create(StopMachineSchema, { name, mode: StopMode.FORCE }) };
    case "pause":
      return { case: "pauseMachine", value: create(PauseMachineSchema, { name }) };
    case "resume":
      return { case: "resumeMachine", value: create(ResumeMachineSchema, { name }) };
    case "checkpoint":
      return {
        case: "takeCheckpoint",
        value: create(TakeCheckpointSchema, { name, label: order.label, then: AfterCheckpoint.RESUME }),
      };
    case "roll-back":
      return { case: "rollBack", value: create(RollBackSchema, { name, label: order.label }) };
    case "restore":
      return { case: "restoreMachine", value: create(RestoreMachineSchema, { name, label: order.label }) };
    case "fork":
      return {
        case: "forkMachine",
        value: create(ForkMachineSchema, { source: name, label: order.label, into: order.into }),
      };
    case "new-copy":
      return { case: "runForks", value: create(RunForksSchema, { name, label: order.label, count: 1 }) };
    case "delete":
      return { case: "deleteMachine", value: create(DeleteMachineSchema, { name }) };
    default: {
      const never: never = order;
      return never;
    }
  }
}

/** How an action ended, for the person who asked. */
export type Verdict =
  | { kind: "done"; note: string | null }
  | { kind: "refused"; reason: string }
  /** It may or may not have happened: the person should look. */
  | { kind: "unknown"; reason: string };

/** Reads the daemon's answer to a lifecycle request. */
export function verdict(outcome: Outcome): Verdict {
  switch (outcome.case) {
    case "done":
    case "machineStarted":
    case "machineResumed":
      return { kind: "done", note: null };
    case "machineStopped":
      switch (outcome.value.outcome) {
        case StopOutcome.STOPPED:
          return { kind: "done", note: null };
        case StopOutcome.SHUTDOWN_PENDING:
          return { kind: "done", note: "asked to shut down; it hasn't yet" };
        case StopOutcome.UNSPECIFIED:
          return { kind: "unknown", reason: "the daemon didn't say whether it stopped" };
        default:
          return { kind: "unknown", reason: "an answer this page doesn't know" };
      }
    case "checkpointReport": {
      const result = outcome.value.result;
      switch (result.case) {
        case "completed":
          return result.value.leftPaused !== undefined
            ? { kind: "done", note: `saved, but it may be left paused: ${result.value.leftPaused.message}` }
            : { kind: "done", note: null };
        case "failed":
          return { kind: "refused", reason: result.value.error?.message ?? "the checkpoint failed" };
        case "unknown":
          return { kind: "unknown", reason: result.value.error?.message ?? "the checkpoint's outcome is unknown" };
        case "inProgress":
          return { kind: "unknown", reason: "the checkpoint ended before it finished" };
        case undefined:
          return { kind: "unknown", reason: "an empty checkpoint report" };
        default: {
          const never: never = result;
          return never;
        }
      }
    }
    case "forksRun": {
      const failed = outcome.value.forks.flatMap((fork) =>
        fork.outcome.case === "failure" ? [`${fork.name}: ${fork.outcome.value.message}`] : [],
      );
      return failed.length === 0 ? { kind: "done", note: null } : { kind: "refused", reason: failed.join("; ") };
    }
    case "machineFailure":
      // A certainty this page doesn't know reads as maybe done.
      return outcome.value.certainty === Certainty.NOT_DONE
        ? { kind: "refused", reason: refusal(outcome) }
        : { kind: "unknown", reason: refusal(outcome) };
    case "error":
    case undefined:
      return { kind: "refused", reason: refusal(outcome) };
    default:
      return { kind: "unknown", reason: refusal(outcome) };
  }
}

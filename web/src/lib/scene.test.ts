/// <reference types="node" />
// The Rust viewer's scene, pinned under fixtures/scene/ by
// crates/doppel-cli/src/view/fixtures.rs: the same wire bytes in, and the
// same moments, scenes, tree rows and elements under points out here, so
// the two viewers can't drift apart unnoticed.

import { readFileSync } from "node:fs";
import { fromBinary } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { AxSnapshotSchema } from "../gen/doppel/agent/v1/agent_pb";
import { ActivitySchema } from "../gen/doppel/daemon/v1/daemon_pb";
import { type Scene, parseActivity } from "./overlay";
import { type Moment, lanes, record, sceneAt } from "./timeline";
import { elementAt, initiallyFolded, readSnapshot, rows } from "./tree";

function fixture(path: string): Uint8Array {
  return readFileSync(new URL(`../../../fixtures/${path}`, import.meta.url));
}

function json<T>(path: string): T {
  return JSON.parse(new TextDecoder().decode(fixture(path))) as T;
}

interface Timeline {
  scenarios: {
    name: string;
    events: { at: number; activity: string }[];
    moments: unknown[];
    lanes: unknown[];
    scenes: { at: number; scene: unknown }[];
  }[];
}

/** A scene in the fixture's shape: what is drawn, without the page's own ids and keys. */
function shape(scene: Scene) {
  return {
    agents: scene.agents.map((agent) => ({
      session: agent.session,
      client: agent.client,
      cursor: agent.cursor,
      current: agent.current === null ? null : { caption: agent.current.call.caption, gesture: agent.current.call.gesture, since: agent.current.since },
      scan: agent.scan,
      marks: agent.marks.map(({ id: _, ...mark }) => mark),
      active: agent.active,
    })),
    callouts: scene.callouts.map((shown) => ({ callout: shown.callout, since: shown.since, until: shown.until })),
  };
}

describe("the timeline agrees with the Rust viewer's", () => {
  for (const scenario of json<Timeline>("scene/timeline.v1.json").scenarios) {
    describe(scenario.name, () => {
      const moments = scenario.events.reduce<readonly Moment[]>((moments, event) => {
        const read = parseActivity(fromBinary(ActivitySchema, Buffer.from(event.activity, "base64")));
        return read === null ? moments : record(moments, read, event.at);
      }, []);

      it("records the same moments", () => {
        expect(
          moments.map((moment) => ({
            caption: moment.call.caption,
            gesture: moment.call.gesture,
            start: moment.startMs,
            end: moment.endMs,
            failed: moment.failed,
          })),
        ).toEqual(scenario.moments);
        expect(
          lanes(moments).map((lane) => ({ session: lane.session, client: lane.client, captions: lane.moments.map((moment) => moment.call.caption) })),
        ).toEqual(scenario.lanes);
      });

      for (const { at, scene } of scenario.scenes) {
        it(`draws the same scene at ${at} ms`, () => {
          expect(shape(sceneAt(moments, at))).toEqual(scene);
        });
      }
    });
  }
});

type Row = [number, number, number | null, boolean, boolean, boolean, boolean[]];

interface Tree {
  rows: Row[];
  at: (number | null)[][];
}

interface Elements {
  snapshots: {
    snapshot: string;
    elements: unknown[];
    folded: number[];
    grid: { columns: number; rows: number; step: number };
    hide: Tree;
    show: Tree;
  }[];
}

describe("the elements agree with the Rust viewer's", () => {
  for (const pinned of json<Elements>("scene/elements.v1.json").snapshots) {
    describe(pinned.snapshot, () => {
      const seen = readSnapshot(fromBinary(AxSnapshotSchema, fixture(pinned.snapshot)));
      const folded = initiallyFolded(seen);

      it("reads the same elements", () => {
        expect(
          seen.elements.map((element) => ({
            index: element.index,
            kind: element.kind,
            name: element.name,
            value: element.value,
            wrapper: element.wrapper,
            seen: element.seen,
          })),
        ).toEqual(pinned.elements);
        expect([...folded].sort((a, b) => a - b)).toEqual(pinned.folded);
      });

      for (const wrappers of ["hide", "show"] as const) {
        it(`shows the same rows with wrappers ${wrappers === "hide" ? "hidden" : "shown"}`, () => {
          expect(rows(seen, wrappers, folded).map((row): Row => [row.index, row.depth, row.parent, row.branch, row.open, row.last, row.lines])).toEqual(
            pinned[wrappers].rows,
          );
        });

        it(`finds the same element under each point with wrappers ${wrappers === "hide" ? "hidden" : "shown"}`, () => {
          const { columns, rows: lines, step } = pinned.grid;
          const grid = Array.from({ length: lines }, (_, row) =>
            Array.from({ length: columns }, (_, column) => elementAt(seen, { x: column * step + step / 2, y: row * step + step / 2 }, wrappers)),
          );
          expect(grid).toEqual(pinned[wrappers].at);
        });
      }
    });
  }
});

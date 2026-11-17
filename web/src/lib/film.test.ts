import { readFileSync } from "node:fs";
import { create, toBinary } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { ActivitySchema, Gesture } from "../gen/doppel/daemon/v1/daemon_pb";
import { Films, FilmError } from "./film";

// The Rust film as the page runs it: `pnpm run film` builds it into src/gen.
const films = await Films.instantiate(readFileSync(new URL("../gen/film.wasm", import.meta.url)));

function click(request: bigint, x: number, y: number, why: string): Uint8Array {
  return toBinary(
    ActivitySchema,
    create(ActivitySchema, {
      session: 1n,
      request,
      machine: "live",
      client: "claude",
      caption: "click",
      phase: { case: "started", value: { gesture: Gesture.CLICK, why, target: { case: "point", value: { x, y } } } },
    }),
  );
}

describe("the film", () => {
  it("draws a live call's pointer and says why beside it", () => {
    const film = films.live();
    film.told(click(1n, 200, 100, "open the menu"), 1000);
    const frame = film.frame(1280, 800, 2000);
    const tip = frame.picture?.shapes.find((shape) => shape.kind.case === "pointer");
    expect(tip?.kind.case === "pointer" && tip.kind.value.tip).toMatchObject({ x: 200, y: 100 });
    expect(frame.captions.map((caption) => caption.why)).toEqual(["open the menu"]);
    film.drop();
  });

  it("refuses what it can't read, saying why", () => {
    const film = films.live();
    expect(() => film.told(new Uint8Array([0xff, 0xff]), 0)).toThrow(FilmError);
    expect(() => film.frame(0, 0, 0)).toThrow(/without area/);
    expect(film.over({ width: 0, height: 800 }, 0)).toBeNull();
    expect(() => films.replay(new Uint8Array(), "Not A Name")).toThrow(/name/);
    film.drop();
  });
});

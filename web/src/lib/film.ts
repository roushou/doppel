// A machine's film: the shapes an exported film burns in, worked out by the
// Rust film (`doppel_scene::film`) built as WebAssembly, so this page, the
// desktop viewer and the films all draw the same. Live, a film takes in the
// daemon's activity events as they come; in replay, a recording's trace and
// each snapshot fetched for it. Each frame it answers what goes over the
// screen, which `Overlay.svelte` draws. The module's exports are parsed here,
// once; past `Films.instantiate`, a film's calls are typed.

import { fromBinary } from "@bufbuild/protobuf";
import { type Frame, FrameSchema } from "../gen/doppel/film/v1/film_pb";
import type { Size } from "./geometry";

/** The module's exports, as `crates/doppel-browser-film/src/exports.rs` makes them. */
interface Exports {
  memory: WebAssembly.Memory;
  film_input(len: number): number;
  film_output(): number;
  film_live(): number;
  film_replay(machineLen: number): number;
  film_told(film: number, now: number): number;
  film_seen(film: number, line: number): number;
  film_frame(film: number, width: number, height: number, now: number): number;
  film_drop(film: number): void;
}

const FUNCTIONS = ["film_input", "film_output", "film_live", "film_replay", "film_told", "film_seen", "film_frame", "film_drop"] as const;

/** The exports of `instance`, or why they aren't the film's. */
function exportsOf(instance: WebAssembly.Instance): Exports {
  const found = instance.exports;
  if (!(found["memory"] instanceof WebAssembly.Memory)) throw new Error("the film's module has no memory");
  for (const name of FUNCTIONS) if (typeof found[name] !== "function") throw new Error(`the film's module has no ${name}`);
  return found as unknown as Exports;
}

/** Why the film refused something, in its words. */
export class FilmError extends Error {}

/** The film's module, loaded: it makes films. */
export class Films {
  readonly #exports: Exports;

  private constructor(exports: Exports) {
    this.#exports = exports;
  }

  /** The module from its bytes, or a response that brings them. */
  static async instantiate(source: BufferSource | Response | Promise<Response>): Promise<Films> {
    const { instance } =
      source instanceof Response || source instanceof Promise
        ? await WebAssembly.instantiateStreaming(source)
        : await WebAssembly.instantiate(source);
    return new Films(exportsOf(instance));
  }

  /** A live machine's film, before anything has happened on it. */
  live(): Film {
    return new Film(this.#exports, this.#exports.film_live());
  }

  /**
   * The film of `machine`'s calls in a recording, from its
   * `doppel.daemon.v1.Recording` bytes.
   */
  replay(recording: Uint8Array, machine: string): Film {
    const name = new TextEncoder().encode(machine);
    const input = new Uint8Array(name.length + recording.length);
    input.set(name);
    input.set(recording, name.length);
    write(this.#exports, input);
    return new Film(this.#exports, answered(this.#exports, this.#exports.film_replay(name.length)));
  }
}

/** Puts `bytes` where the module reads its next call's input. */
function write(exports: Exports, bytes: Uint8Array): void {
  const at = exports.film_input(bytes.length) >>> 0;
  // Made after the call: room for the input may have grown the memory.
  new Uint8Array(exports.memory.buffer, at, bytes.length).set(bytes);
}

/** The output of a call that answered `len`; a refusal throws why. */
function answered(exports: Exports, len: number): number {
  if (len >= 0) return len;
  throw new FilmError(new TextDecoder().decode(output(exports, -len)));
}

function output(exports: Exports, len: number): Uint8Array {
  return new Uint8Array(exports.memory.buffer, exports.film_output() >>> 0, len).slice();
}

/** One machine's film. Let it go with `drop` once nothing draws it. */
export class Film {
  readonly #exports: Exports;
  #handle: number;

  constructor(exports: Exports, handle: number) {
    this.#exports = exports;
    this.#handle = handle;
  }

  /** Takes in a `doppel.daemon.v1.Activity` the daemon sent about this film's machine, at `now`. */
  told(activity: Uint8Array, now: number): void {
    write(this.#exports, activity);
    answered(this.#exports, this.#exports.film_told(this.#handle, now));
  }

  /** Puts the `doppel.agent.v1.AxSnapshot` a recorded call saw in place, by its trace line. */
  seen(line: number, snapshot: Uint8Array): void {
    write(this.#exports, snapshot);
    answered(this.#exports, this.#exports.film_seen(this.#handle, line));
  }

  /** What goes over a display of `width` × `height` points at `now`. */
  frame(width: number, height: number, now: number): Frame {
    const len = answered(this.#exports, this.#exports.film_frame(this.#handle, width, height, now));
    return fromBinary(FrameSchema, output(this.#exports, len));
  }

  /** What goes over `display` at `now`; none over a display without area. */
  over(display: Size, now: number): Frame | null {
    if (!(display.width > 0 && display.height > 0)) return null;
    return this.frame(display.width, display.height, now);
  }

  /** Lets the film go: it draws nothing more. */
  drop(): void {
    this.#exports.film_drop(this.#handle);
    this.#handle = 0;
  }
}

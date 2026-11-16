// A machine's screen from its H.264 stream, decoded by WebCodecs and drawn
// on a canvas. Frames before the first key frame, and after a decoding
// error until the next key frame, can't be decoded and are skipped.
//
// The software decoder is preferred: a hardware one may hold a frame back
// until the next arrives, and a still screen sends no next, so the picture
// would never show. Where software decoding isn't available, the page falls
// back to whatever the browser has.
//
// The decoder can fall behind the stream, on a slow device or a busy tab:
// the frames it hasn't decoded yet pile up, and the picture lags further
// and further behind the screen. Past `MAX_QUEUED` frames waiting, the
// decoder drops them and asks for the stream again, which starts with a
// key frame of the screen as it is.

import type { EncodedFrame } from "../gen/doppel/vmm/v1/vmm_pb";
import { avcC, codecString } from "./avc";

/** How many frames may wait to be decoded before the picture is too far behind and starts over. */
export const MAX_QUEUED = 6;

/** What to do with a frame that arrives. */
export type Step =
  /** Decode it, configuring the decoder first when it's a key frame with new parameters. */
  | { kind: "decode"; configure: boolean }
  /** It can't be decoded: no key frame since the decoder started or caught up. */
  | { kind: "skip" }
  /** Too many frames wait: drop them, and start again from a key frame. */
  | { kind: "catch-up" };

/**
 * The step for a frame: `parameters` identifies a key frame's parameter
 * sets (`null` for a delta frame), `configured` the decoder's (`null` before
 * its first key frame), and `queued` counts the frames waiting.
 */
export function step(parameters: string | null, configured: string | null, queued: number): Step {
  if (queued >= MAX_QUEUED) return { kind: "catch-up" };
  if (parameters !== null) return { kind: "decode", configure: parameters !== configured };
  return configured === null ? { kind: "skip" } : { kind: "decode", configure: false };
}

export class ScreenDecoder {
  /** Which decoder to ask for: software until the browser refuses it, for the page's life. */
  static #acceleration: HardwareAcceleration = "prefer-software";
  readonly #canvas: HTMLCanvasElement;
  readonly #failed: (error: Error) => void;
  readonly #drawn: () => void;
  readonly #behind: () => void;
  #decoder: VideoDecoder;
  /** The configuration in use, to reconfigure only when a key frame's differs. */
  #configured: string | null = null;

  /**
   * `drawn` is told each time a frame is on the canvas; `failed`, when
   * decoding fails; `behind`, when the decoder dropped its backlog and needs
   * the stream again from a key frame.
   */
  constructor(canvas: HTMLCanvasElement, failed: (error: Error) => void, drawn: () => void, behind: () => void) {
    this.#canvas = canvas;
    this.#failed = failed;
    this.#drawn = drawn;
    this.#behind = behind;
    this.#decoder = this.#make();
  }

  /** Whether this browser can decode the stream at all. */
  static supported(): boolean {
    return typeof VideoDecoder === "function";
  }

  #make(): VideoDecoder {
    return new VideoDecoder({
      output: (frame) => this.#draw(frame),
      error: (error) => {
        this.#configured = null;
        if (error.name === "NotSupportedError") ScreenDecoder.#acceleration = "no-preference";
        this.#failed(error);
      },
    });
  }

  #draw(frame: VideoFrame): void {
    const canvas = this.#canvas;
    if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
      canvas.width = frame.displayWidth;
      canvas.height = frame.displayHeight;
    }
    canvas.getContext("2d")?.drawImage(frame, 0, 0);
    frame.close();
    this.#drawn();
  }

  push(frame: EncodedFrame): void {
    if (this.#decoder.state === "closed") this.#decoder = this.#make();
    let parameters: { sps: Uint8Array; description: Uint8Array; key: string } | null = null;
    if (frame.key) {
      const [sps, pps] = frame.parameterSets;
      if (sps === undefined || pps === undefined) return;
      const description = avcC(sps, pps);
      parameters = { sps, description, key: Array.from(description).join(",") };
    }
    const next = step(parameters?.key ?? null, this.#configured, this.#decoder.decodeQueueSize);
    switch (next.kind) {
      case "skip":
        return;
      case "catch-up":
        // Frames still to come are skipped until a key frame configures it again.
        this.#decoder.reset();
        this.#configured = null;
        this.#behind();
        return;
      case "decode":
        break;
      default: {
        const never: never = next;
        return never;
      }
    }
    if (next.configure && parameters !== null) {
      this.#decoder.configure({
        codec: codecString(parameters.sps),
        description: parameters.description,
        optimizeForLatency: true,
        hardwareAcceleration: ScreenDecoder.#acceleration,
      });
      this.#configured = parameters.key;
    }
    this.#decoder.decode(
      new EncodedVideoChunk({
        type: frame.key ? "key" : "delta",
        // Only the order matters: each frame is drawn when decoded.
        timestamp: Number(frame.sequence),
        data: frame.data,
      }),
    );
  }

  close(): void {
    if (this.#decoder.state !== "closed") this.#decoder.close();
  }
}

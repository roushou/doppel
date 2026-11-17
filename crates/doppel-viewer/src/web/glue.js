// The browser APIs the viewer reaches from Rust, each as small as it can
// be: a screen's decoder (WebCodecs), a recording's player (a <video>),
// the page's token, its local time, and the theme it keeps. Frames come
// back as RGBA pixels, which the Rust side hands to GPUI.

/** Draws `source` on a canvas of its size and reads its pixels back. */
class Grabber {
  constructor() {
    this.canvas = null;
    this.context = null;
  }

  /** RGBA pixels of `source`, `width` × `height`. */
  grab(source, width, height) {
    if (this.canvas === null || this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas = new OffscreenCanvas(width, height);
      this.context = this.canvas.getContext("2d", { willReadFrequently: true });
    }
    this.context.drawImage(source, 0, 0, width, height);
    const pixels = this.context.getImageData(0, 0, width, height).data;
    return new Uint8Array(pixels.buffer, pixels.byteOffset, pixels.byteLength);
  }
}

/**
 * A screen's H.264 decoder. The software decoder is preferred: a hardware
 * one may hold a frame back until the next arrives, and a still screen
 * sends no next, so the picture would never show. Where software decoding
 * isn't available, the next configuration takes whatever the browser has.
 */
export class ScreenDecoder {
  constructor(onFrame, onError) {
    this.onFrame = onFrame;
    this.onError = onError;
    this.decoder = null;
    this.grabber = new Grabber();
    this.software = true;
  }

  /** Starts decoding a stream of `codec` with its `avcC` record; false if it can't. */
  configure(codec, description) {
    this.close();
    const config = { codec, description, optimizeForLatency: true };
    if (this.software) config.hardwareAcceleration = "prefer-software";
    this.decoder = new VideoDecoder({
      output: (frame) => this.output(frame),
      error: (error) => {
        this.software = false;
        this.onError(String(error));
      },
    });
    try {
      this.decoder.configure(config);
      return true;
    } catch (error) {
      this.software = false;
      this.onError(String(error));
      return false;
    }
  }

  /** Decodes one frame; false when it can't take it (not configured, or too far behind). */
  decode(data, key, timestamp) {
    const decoder = this.decoder;
    if (decoder === null || decoder.state !== "configured" || decoder.decodeQueueSize > 6) return false;
    decoder.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp, data }));
    return true;
  }

  output(frame) {
    const width = frame.displayWidth;
    const height = frame.displayHeight;
    let pixels;
    try {
      pixels = this.grabber.grab(frame, width, height);
    } finally {
      frame.close();
    }
    this.onFrame(width, height, pixels);
  }

  close() {
    if (this.decoder !== null && this.decoder.state !== "closed") this.decoder.close();
    this.decoder = null;
  }
}

/** A recording's video, played by a <video> the page never shows. */
export class VideoPlayer {
  constructor(url, onFrame, onEnded, onError) {
    this.onFrame = onFrame;
    this.grabber = new Grabber();
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.preload = "auto";
    video.addEventListener("ended", () => onEnded());
    video.addEventListener("error", () => onError(video.error?.message || "the video can't be played"));
    video.addEventListener("seeked", () => this.grab());
    video.addEventListener("loadeddata", () => this.grab());
    this.video = video;
    this.closed = false;
    const next = () => {
      if (this.closed) return;
      this.grab();
      video.requestVideoFrameCallback(next);
    };
    video.requestVideoFrameCallback(next);
    video.src = url;
  }

  grab() {
    const video = this.video;
    if (this.closed || video.videoWidth === 0 || video.readyState < 2) return;
    this.onFrame(video.videoWidth, video.videoHeight, this.grabber.grab(video, video.videoWidth, video.videoHeight));
  }

  play() {
    this.video.play().catch(() => {});
  }

  pause() {
    this.video.pause();
  }

  seek(seconds) {
    this.video.currentTime = seconds;
  }

  /** Where the playhead is, in seconds. */
  position() {
    return this.video.currentTime;
  }

  playing() {
    return !this.video.paused && !this.video.ended;
  }

  /** Its length in seconds; negative until it's known. */
  duration() {
    const duration = this.video.duration;
    return Number.isFinite(duration) ? duration : -1;
  }

  close() {
    this.closed = true;
    this.video.pause();
    this.video.removeAttribute("src");
    this.video.load();
  }
}

const TOKEN_KEY = "doppel-token";

/**
 * The token the page was opened with: in the link after `#token=`, which
 * browsers never send to a server. Read once and removed from the address
 * bar, then kept for this tab only, so a reload still works. Empty when
 * there's none.
 */
export function takeToken() {
  const params = new URLSearchParams(location.hash.replace(/^#/, ""));
  const raw = params.get("token");
  if (raw !== null) {
    params.delete("token");
    const rest = params.toString();
    history.replaceState(null, "", location.pathname + location.search + (rest === "" ? "" : `#${rest}`));
    try {
      sessionStorage.setItem(TOKEN_KEY, raw);
    } catch {}
    return raw;
  }
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

/**
 * Gives recordings' videos the token: a <video> can't send a header, so
 * the gateway reads it from this cookie. Only for `/recordings/`, only over
 * HTTPS, never sent from another site's page, and gone when the browser quits.
 */
export function shareTokenWithVideos(token) {
  document.cookie = `doppel-token=${token}; Path=/recordings/; Secure; SameSite=Strict`;
}

/** The daemon's WebSocket at this page's own origin. */
export function socketUrl() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/socket`;
}

/** Monotonic milliseconds, from the page's start. */
export function millis() {
  return performance.now();
}

/** `unixMs` in the browser's time zone: year, month, day, hour, minute, second. */
export function localParts(unixMs) {
  const at = new Date(unixMs);
  return new Int32Array([at.getFullYear(), at.getMonth() + 1, at.getDate(), at.getHours(), at.getMinutes(), at.getSeconds()]);
}

const THEME_KEY = "doppel-theme";

/** The theme kept last; empty when none was. */
export function keptTheme() {
  try {
    return localStorage.getItem(THEME_KEY) ?? "";
  } catch {
    return "";
  }
}

/** Keeps the theme; false when the browser won't. */
export function keepTheme(theme) {
  try {
    localStorage.setItem(THEME_KEY, theme);
    return true;
  } catch {
    return false;
  }
}

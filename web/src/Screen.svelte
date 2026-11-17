<script lang="ts">
  import { Keyboard, LoaderCircle } from "@lucide/svelte";
  import { onDestroy, onMount, type Snippet } from "svelte";
  import { create } from "@bufbuild/protobuf";
  import { type Input, PointerPhase } from "./gen/doppel/common/v1/common_pb";
  import { OpenEncodedDisplaySchema } from "./gen/doppel/daemon/v1/daemon_pb";
  import { Connection, refusal } from "./lib/connection";
  import { ScreenDecoder } from "./lib/decoder";
  import { contain, locate, type Rect, type Size } from "./lib/geometry";
  import { editInput, keyInput } from "./lib/keys";
  import type { MachineName } from "./lib/machines";
  import { button, Clicks, held, heldButton, pointerInput, scrollInput } from "./lib/pointer";
  import type { Token } from "./lib/token";
  import type { Frame } from "./gen/doppel/film/v1/film_pb";
  import Overlay from "./Overlay.svelte";

  interface Props {
    name: MachineName;
    token: Token;
    /** Whether input goes to this machine: this page has control of it. */
    controlling: boolean;
    send: (input: Input) => void;
    /** What agents do on it, drawn over the picture: its film's frame for the display's size; none to draw nothing. */
    film?: ((display: Size) => Frame | null) | null;
    /** Drawn over the picture, under the agent's overlay: given where the picture is and the display's size. */
    layer?: Snippet<[Rect, Size]> | undefined;
    /** Where the picture is drawn in the screen's box, for what frames it; null until a frame says. */
    picture?: Rect | null;
  }

  let { name, token, controlling, send, film = null, layer, picture = $bindable(null) }: Props = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  /** The field an on-screen keyboard types into: its edits are sent, never kept. */
  let field: HTMLInputElement | undefined = $state();
  /** Why the screen isn't showing, if it isn't. */
  let problem: string | null = $state(null);
  /** Whether a frame is drawn yet: until then the screen says it's waiting, not black. */
  let pictured = $state(false);
  let display: Size | null = $state(null);
  let width = $state(0);
  let height = $state(0);
  /** Where the picture is drawn in the screen's box. */
  const drawn = $derived(display === null ? null : contain({ left: 0, top: 0, width, height }, display));
  $effect(() => {
    picture = drawn;
  });
  let connection: Connection | null = null;
  let decoder: ScreenDecoder | null = null;
  let ended = false;
  /** The decoder fell behind: its stream is opened again at once, from a key frame. */
  let catchingUp = false;
  /** The latest hover move, sent once per animation frame. */
  let move: Input | null = null;
  let moveQueued = false;
  const clicks = new Clicks();

  async function open(): Promise<void> {
    if (canvas === undefined || ended) return;
    if (!ScreenDecoder.supported()) {
      problem = "this browser can't decode video (WebCodecs)";
      return;
    }
    decoder?.close();
    decoder = new ScreenDecoder(
      canvas,
      (error) => {
        problem = `decoding failed (${error.message}); reconnecting…`;
        connection?.close();
      },
      () => (pictured = true),
      () => {
        catchingUp = true;
        connection?.close();
      },
    );
    try {
      connection = await Connection.open(Connection.socketUrl(location), token);
    } catch (error) {
      problem = `can't connect: ${error instanceof Error ? error.message : String(error)}`;
      return;
    }
    connection.onClose(() => {
      // The display's connection ends when its machine stops, or failed:
      // try again shortly, unless the screen went away.
      if (ended) return;
      if (catchingUp) {
        catchingUp = false;
        void open();
        return;
      }
      problem ??= "the screen closed; reconnecting…";
      setTimeout(() => void open(), 2000);
    });
    const outcome = await connection.stream(
      { case: "openEncodedDisplay", value: create(OpenEncodedDisplaySchema, { name }) },
      (event) => {
        if (event.kind.case === "encodedFrame") {
          problem = null;
          decoder?.push(event.kind.value);
        }
      },
    );
    if (outcome.case === "encodedDisplay") {
      const points = outcome.value.displayPoints;
      display = points === undefined ? null : { width: points.width, height: points.height };
    } else {
      problem = `no screen: ${refusal(outcome)}`;
    }
  }

  onMount(() => void open());
  onDestroy(() => {
    ended = true;
    connection?.close();
    decoder?.close();
  });

  /** The display point under a page point. */
  function at(event: MouseEvent, edge: "clamp" | "off") {
    if (canvas === undefined || display === null) return null;
    const box = canvas.getBoundingClientRect();
    const drawn = contain(
      { left: box.left, top: box.top, width: box.width, height: box.height },
      { width: canvas.width, height: canvas.height },
    );
    return drawn === null ? null : locate(drawn, display, event.clientX, event.clientY, edge);
  }

  function pressed(event: PointerEvent, phase: PointerPhase.PRESS | PointerPhase.RELEASE): void {
    if (!controlling) return;
    const which = button(event.button);
    const point = at(event, phase === PointerPhase.PRESS ? "off" : "clamp");
    if (which === null || point === null) return;
    event.preventDefault();
    let count: 1 | 2 | 3;
    if (phase === PointerPhase.PRESS) {
      canvas?.setPointerCapture(event.pointerId);
      canvas?.focus();
      count = clicks.press({ button: which, at: { x: event.clientX, y: event.clientY }, time: event.timeStamp });
    } else {
      count = clicks.release();
    }
    send(pointerInput(phase, point, which, count, held(event)));
  }

  function moved(event: PointerEvent): void {
    if (!controlling) return;
    const dragging = event.buttons !== 0;
    const point = at(event, dragging ? "clamp" : "off");
    if (point === null) return;
    move = pointerInput(PointerPhase.MOVE, point, heldButton(event.buttons), null, held(event));
    if (!moveQueued) {
      moveQueued = true;
      requestAnimationFrame(() => {
        moveQueued = false;
        if (move !== null) send(move);
        move = null;
      });
    }
  }

  function scrolled(event: WheelEvent): void {
    if (!controlling || canvas === undefined || display === null) return;
    const point = at(event, "off");
    if (point === null) return;
    event.preventDefault();
    // Page pixels to display points.
    const scale = display.height / canvas.getBoundingClientRect().height;
    const lines = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? 16 : 1;
    send(scrollInput(point, event.deltaX * lines * scale, event.deltaY * lines * scale));
  }

  function typed(event: KeyboardEvent): void {
    if (!controlling || event.isComposing) return;
    const input = keyInput(event);
    if (input === null) return;
    event.preventDefault();
    send(input);
  }

  // An on-screen keyboard's presses come as edits to the field, except
  // the keys it reports as presses, which `typed` sends and cancels.
  function edited(event: InputEvent): void {
    if (!controlling) return;
    const inputs = editInput(event.inputType, event.data);
    if (event.cancelable && !event.isComposing) event.preventDefault();
    for (const input of inputs) send(input);
  }

  function composed(event: CompositionEvent): void {
    if (controlling) for (const input of editInput("insertText", event.data)) send(input);
    if (field !== undefined) field.value = "";
  }

  function settled(event: Event): void {
    if (field !== undefined && !(event as InputEvent).isComposing) field.value = "";
  }
</script>

<div class={["screen", { controlling }]} bind:clientWidth={width} bind:clientHeight={height}>
  <canvas
    bind:this={canvas}
    tabindex={controlling ? 0 : -1}
    aria-label={`${name}'s screen`}
    onpointerdown={(event) => pressed(event, PointerPhase.PRESS)}
    onpointerup={(event) => pressed(event, PointerPhase.RELEASE)}
    onpointermove={moved}
    onwheel={scrolled}
    onkeydown={typed}
    oncontextmenu={(event) => controlling && event.preventDefault()}
  ></canvas>
  {#if controlling}
    <input
      bind:this={field}
      class="field"
      aria-label={`Type on ${name}`}
      autocomplete="off"
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
      enterkeyhint="enter"
      onkeydown={typed}
      onbeforeinput={edited}
      oninput={settled}
      oncompositionend={composed}
    />
    <button class="keyboard" onclick={() => field?.focus()}><Keyboard size={14} />Keyboard</button>
  {/if}
  {#if layer !== undefined && drawn !== null && display !== null && problem === null}
    {@render layer(drawn, display)}
  {/if}
  {#if film !== null && drawn !== null && display !== null && problem === null}
    {@const frame = film(display)}
    {#if frame !== null}<Overlay {frame} {drawn} {display} />{/if}
  {/if}
  {#if problem !== null}
    <p class="problem">{problem}</p>
  {:else if !pictured}
    <p class="waiting" role="status"><LoaderCircle size={16} class="spin" /><span>Waiting for the screen</span></p>
  {/if}
</div>

<style>
  /* Fills its frame; the picture is fitted inside, aspect kept. */
  .screen {
    position: absolute;
    inset: 0;
    background: var(--screen);
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    outline: none;
  }
  /* A touch on a screen you control goes to the guest, not to scrolling. */
  .controlling canvas {
    touch-action: none;
  }
  /* Out of sight but focusable, so a phone shows its keyboard for it. */
  .field {
    position: absolute;
    left: 0;
    bottom: 0;
    width: 1px;
    height: 1px;
    opacity: 0;
    border: 0;
    padding: 0;
    font-size: 16px;
  }
  .keyboard {
    position: absolute;
    top: 10px;
    right: 10px;
    background: color-mix(in srgb, var(--overlay) 85%, transparent);
    backdrop-filter: blur(8px);
  }
  /* A mouse and a hardware keyboard type on the screen itself. */
  @media (pointer: fine) {
    .keyboard {
      display: none;
    }
  }
  .waiting {
    position: absolute;
    inset: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    font: 500 11px/1 var(--mono);
    letter-spacing: var(--track);
    text-transform: uppercase;
    color: var(--muted);
    pointer-events: none;
  }
  .problem {
    position: absolute;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    margin: 0;
    padding: 5px 12px;
    border-radius: 0;
    font-size: 12px;
    color: var(--muted);
    background: color-mix(in srgb, var(--overlay) 85%, transparent);
    backdrop-filter: blur(8px);
    white-space: nowrap;
  }
</style>

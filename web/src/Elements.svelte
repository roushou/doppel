<script lang="ts">
  import { locate, type Rect, type Size } from "./lib/geometry";
  import { type Element, type Kind, type Seen, elementAt } from "./lib/tree";

  interface Props {
    seen: Seen;
    /** The refs the agent's model was given for its elements; `null` when it named none. */
    refs: ReadonlyMap<number, string> | null;
    /** Where the picture is drawn in the screen's box, in CSS pixels. */
    drawn: Rect;
    /** The display's size, in points. */
    display: Size;
    wrappers: "show" | "hide";
    /** The element under the pointer, here or in the tree. */
    hovered: number | null;
    /** The element picked, here or in the tree. */
    selected: number | null;
    /** The screen may have changed since the snapshot: its boxes are dimmed. */
    stale: boolean;
    /** Whether a pointer over the screen picks elements; off while a person drives it. */
    picking: boolean;
    onhover: (index: number | null) => void;
    onselect: (index: number | null) => void;
  }

  let { seen, refs, drawn, display, wrappers, hovered, selected, stale, picking, onhover, onselect }: Props = $props();

  let layer: HTMLDivElement | undefined = $state();

  function box(frame: Rect): Rect {
    return {
      left: drawn.left + (frame.left / display.width) * drawn.width,
      top: drawn.top + (frame.top / display.height) * drawn.height,
      width: (frame.width / display.width) * drawn.width,
      height: (frame.height / display.height) * drawn.height,
    };
  }

  /** Kinds outlined all the time: what an agent can act on. The rest show when pointed at. */
  function outlined(kind: Kind): boolean {
    switch (kind) {
      case "control":
      case "field":
      case "menu":
        return true;
      case "window":
      case "text":
      case "container":
      case "other":
        return false;
      default: {
        const never: never = kind;
        return never;
      }
    }
  }

  const shown = $derived(
    seen.elements.filter(
      (element): element is Element & { seen: Rect } =>
        element.seen !== null &&
        !(wrappers === "hide" && element.wrapper) &&
        (outlined(element.kind) || element.index === hovered || element.index === selected),
    ),
  );

  function at(event: MouseEvent): number | null {
    if (layer === undefined) return null;
    const bounds = layer.getBoundingClientRect();
    const point = locate(drawn, display, event.clientX - bounds.left, event.clientY - bounds.top, "off");
    return point === null ? null : elementAt(seen, point, wrappers);
  }

  function label(element: Element): string {
    const words = element.name === "" ? element.roleWords : `${element.roleWords} “${element.name}”`;
    const named = refs?.get(element.index);
    return named === undefined ? words : `${named} · ${words}`;
  }
</script>

<div
  bind:this={layer}
  class={["elements", { stale, picking }]}
  role="presentation"
  onpointermove={(event) => picking && onhover(at(event))}
  onpointerleave={() => picking && onhover(null)}
  onclick={(event) => picking && onselect(at(event))}
>
  {#each shown as element (element.index)}
    {@const rect = box(element.seen)}
    {@const focus = element.index === hovered || element.index === selected}
    <div
      class={["box", element.kind, { focus, chosen: element.index === selected }]}
      style:left="{rect.left}px"
      style:top="{rect.top}px"
      style:width="{rect.width}px"
      style:height="{rect.height}px"
    >
      {#if focus}<span class={["tag", { below: rect.top < 22 }]}>{label(element)}</span>{/if}
    </div>
  {/each}
</div>

<style>
  .elements {
    position: absolute;
    inset: 0;
    pointer-events: none;
    overflow: hidden;
    transition: opacity 0.2s var(--ease);
    /* Kind colours, chosen to read over any guest content: each box also has a dark halo. */
    --kind-window: #9097a2;
    --kind-control: #4c8dff;
    --kind-field: #2fbf71;
    --kind-text: #b18cff;
    --kind-container: #9097a2;
    --kind-menu: #f0b429;
    --kind-other: #9097a2;
  }
  .elements.picking {
    pointer-events: auto;
    cursor: crosshair;
  }
  .elements.stale .box:not(.focus) {
    opacity: 0.35;
  }
  .box {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid var(--kind);
    border-radius: 0;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.45);
    opacity: 0.7;
  }
  .box.focus {
    opacity: 1;
    border-width: 2px;
    background: color-mix(in srgb, var(--kind) 18%, transparent);
  }
  .box.chosen {
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.5),
      0 0 0 4px color-mix(in srgb, var(--kind) 35%, transparent);
  }
  .window {
    --kind: var(--kind-window);
  }
  .control {
    --kind: var(--kind-control);
  }
  .field {
    --kind: var(--kind-field);
  }
  .text {
    --kind: var(--kind-text);
  }
  .container {
    --kind: var(--kind-container);
    border-style: dashed;
  }
  .menu {
    --kind: var(--kind-menu);
  }
  .other {
    --kind: var(--kind-other);
  }
  .tag {
    position: absolute;
    left: -2px;
    bottom: calc(100% + 3px);
    max-width: 280px;
    padding: 2px 6px;
    border-radius: 0;
    font: 500 11px/1.4 var(--sans);
    color: #fff;
    background: color-mix(in srgb, var(--kind) 85%, #000);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tag.below {
    bottom: auto;
    top: calc(100% + 3px);
  }
</style>

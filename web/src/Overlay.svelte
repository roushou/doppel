<script lang="ts">
  import type { Component } from "svelte";
  import { ArrowDownUp, Check, Eye, Flag, Hand, Hourglass, Keyboard, MapPin, Pointer, Power, Rewind, Terminal, TextCursorInput } from "@lucide/svelte";
  import type { Frame } from "./gen/doppel/film/v1/film_pb";
  import { type Shape, Glyph, StrokeLine, TagCorner } from "./gen/doppel/vmm/v1/vmm_pb";
  import type { Rect, Size } from "./lib/geometry";
  import { hue } from "./lib/hues";
  import { CAPTION_FROM, Place, arrow, brackets, captionSide, colour, inset, pointer, veil } from "./lib/shapes";

  // A film's frame over a machine's screen: the shapes an exported film
  // burns in, worked out by the film (`lib/film.ts`), and beside each
  // agent's pointer why it makes its call. The desktop viewer's
  // `shapes.rs` and `overlay.rs` draw the same.

  interface Props {
    frame: Frame;
    /** Where the picture is drawn in the screen's box, in CSS pixels. */
    drawn: Rect;
    /** The display's size, in points. */
    display: Size;
  }

  let { frame, drawn, display }: Props = $props();

  const place = $derived(new Place(drawn, display));
  const shapes = $derived(frame.picture?.shapes ?? []);
  /** Badges and tags go over everything the SVG paints. */
  const painted = $derived(shapes.filter((shape) => shape.kind.case !== "glyph" && shape.kind.case !== "tag"));
  const onTop = $derived(shapes.filter((shape) => shape.kind.case === "glyph" || shape.kind.case === "tag"));

  /** The icon a glyph shows on its badge, as the desktop's. */
  function icon(glyph: Glyph): Component<{ size?: number; color?: string }> | null {
    switch (glyph) {
      case Glyph.PRESS:
        return Pointer;
      case Glyph.TYPE:
        return TextCursorInput;
      case Glyph.KEYS:
        return Keyboard;
      case Glyph.COMMAND:
        return Terminal;
      case Glyph.WAIT:
        return Hourglass;
      case Glyph.LOOK:
        return Eye;
      case Glyph.POINT:
        return MapPin;
      case Glyph.DRAG:
        return Hand;
      case Glyph.SCROLL:
        return ArrowDownUp;
      case Glyph.MACHINE:
        return Power;
      case Glyph.CHECKPOINT:
        return Flag;
      case Glyph.ROLLBACK:
        return Rewind;
      case Glyph.DONE:
        return Check;
      // One a newer film draws: the badge alone.
      case Glyph.UNSPECIFIED:
        return null;
      default: {
        const never: never = glyph;
        return never;
      }
    }
  }

  /** How a tag grows away from its corner. */
  function shift(corner: TagCorner): string {
    switch (corner) {
      case TagCorner.TOP_RIGHT:
        return "translateX(-100%)";
      case TagCorner.BOTTOM_LEFT:
        return "translateY(-100%)";
      case TagCorner.TOP_LEFT:
      case TagCorner.UNSPECIFIED:
        return "none";
      default: {
        const never: never = corner;
        return never;
      }
    }
  }

  /** A caption's words: the client, upper case, when several agents act, and why. */
  function words(client: string | undefined, why: string | undefined): string | null {
    const name = client === undefined ? null : client.toUpperCase();
    if (why !== undefined) return name === null ? why : `${name} · ${why}`;
    return name;
  }

  const right = $derived(drawn.left + drawn.width);
</script>

{#snippet paint(shape: Shape)}
  {@const ink = colour(shape.colour, shape.opacity)}
  {#if shape.kind.case === "frame"}
    {@const width = place.length(shape.kind.value.width)}
    {@const rect = inset(place.rect(shape.kind.value.rect), width)}
    <rect
      x={rect.left}
      y={rect.top}
      width={rect.width}
      height={rect.height}
      fill={colour(shape.colour, shape.kind.value.wash * shape.opacity)}
      stroke={ink}
      stroke-width={width}
    />
  {:else if shape.kind.case === "ring"}
    {@const width = place.length(shape.kind.value.width)}
    {@const centre = place.at(shape.kind.value.centre)}
    <circle
      cx={centre.x}
      cy={centre.y}
      r={Math.max(0, place.length(shape.kind.value.radius) - width / 2)}
      fill={colour(shape.colour, shape.kind.value.wash * shape.opacity)}
      stroke={ink}
      stroke-width={width}
    />
  {:else if shape.kind.case === "stroke"}
    {@const from = place.at(shape.kind.value.from)}
    {@const to = place.at(shape.kind.value.to)}
    {@const width = place.length(shape.kind.value.width)}
    {#if shape.kind.value.line === StrokeLine.ARROW}
      {@const tip = arrow(from, to, place.length(14), place.length(8.4))}
      <polygon points={tip.head} fill={ink} />
      <line x1={from.x} y1={from.y} x2={tip.end.x} y2={tip.end.y} stroke={ink} stroke-width={width} />
    {:else}
      <line
        x1={from.x}
        y1={from.y}
        x2={to.x}
        y2={to.y}
        stroke={ink}
        stroke-width={width}
        stroke-dasharray={shape.kind.value.line === StrokeLine.DASHED ? `${place.length(3)} ${place.length(5)}` : undefined}
      />
    {/if}
  {:else if shape.kind.case === "pointer"}
    <polygon
      points={pointer(place.at(shape.kind.value.tip), place.scale)}
      fill={ink}
      stroke={colour(0xffffff, shape.opacity)}
      stroke-width={1.5 * place.scale}
      stroke-linejoin="round"
    />
  {:else if shape.kind.case === "veil"}
    <path d={veil(drawn, shape.kind.value.holes.map((hole) => place.rect(hole)))} fill={ink} fill-rule="evenodd" />
  {:else if shape.kind.case === "brackets"}
    <path
      d={brackets(place.rect(shape.kind.value.rect), place.length(shape.kind.value.arm))}
      fill="none"
      stroke={ink}
      stroke-width={place.length(shape.kind.value.width)}
    />
  {/if}
{/snippet}

<div class="overlay" aria-hidden="true">
  <svg class="painted">
    {#each painted as shape, index (index)}
      {@render paint(shape)}
    {/each}
  </svg>
  {#each onTop as shape, index (index)}
    {#if shape.kind.case === "glyph"}
      {@const side = place.length(shape.kind.value.size)}
      {@const centre = place.at(shape.kind.value.centre)}
      {@const Icon = icon(shape.kind.value.glyph)}
      <span
        class="glyph"
        style:left="{centre.x - side / 2}px"
        style:top="{centre.y - side / 2}px"
        style:width="{side}px"
        style:height="{side}px"
        style:background={colour(shape.colour, 1)}
        style:opacity={shape.opacity}
      >
        {#if Icon !== null}<Icon size={side * 0.55} color="white" />{/if}
      </span>
    {:else if shape.kind.case === "tag"}
      {@const at = place.at(shape.kind.value.at)}
      <span
        class="tag"
        style:left="{at.x}px"
        style:top="{at.y}px"
        style:transform={shift(shape.kind.value.corner)}
        style:padding="{place.length(3)}px {place.length(6)}px"
        style:font-size="{place.length(11)}px"
        style:background={colour(shape.colour, 1)}
        style:color={colour(shape.kind.value.ink, 1)}
        style:opacity={shape.opacity}>{shape.kind.value.text}</span
      >
    {/if}
  {/each}
  {#each frame.captions as caption (caption.session)}
    {@const said = words(caption.client, caption.why)}
    {#if said !== null && caption.tip !== undefined}
      {@const { fill, ink } = hue(Number(caption.session))}
      {@const start = place.at({ x: caption.tip.x + CAPTION_FROM.x, y: caption.tip.y + CAPTION_FROM.y })}
      {#if captionSide(start.x, right) === "right"}
        <span class="caption" style:left="{start.x}px" style:top="{start.y}px" style:background={fill} style:color={ink}>{said}</span>
      {:else}
        {@const end = place.at({ x: caption.tip.x - 8, y: caption.tip.y })}
        <span class="caption" style:right="calc(100% - {end.x}px)" style:top="{start.y}px" style:background={fill} style:color={ink}>{said}</span>
      {/if}
    {/if}
  {/each}
</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    pointer-events: none;
    overflow: hidden;
  }

  .painted {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }

  .glyph {
    position: absolute;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1), 0 2px 4px -2px rgba(0, 0, 0, 0.1);
  }

  .tag {
    position: absolute;
    white-space: nowrap;
    font-family: var(--mono);
    font-weight: 500;
    line-height: 1.2;
  }

  .caption {
    position: absolute;
    max-width: 300px;
    padding: 4px 8px;
    font: 500 11px/1.2 var(--mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>

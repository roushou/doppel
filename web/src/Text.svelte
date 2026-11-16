<script lang="ts">
  import { LoaderCircle, ScanText } from "@lucide/svelte";
  import type { Rect, Size } from "./lib/geometry";
  import type { TextRead } from "./lib/reading";

  interface Props {
    /** What to draw: a read, or why there is none. */
    shown: { kind: "read"; read: TextRead } | { kind: "loading" } | { kind: "failed"; why: string } | { kind: "nothing" };
    /** Whose read it is and how old (`agent · 4 s ago`). */
    whose: string;
    /** The screen may have changed since the read: its boxes are dimmed. */
    stale: boolean;
    /** Where the picture is drawn in the screen's box, in CSS pixels. */
    drawn: Rect;
    /** The display's size, in points. */
    display: Size;
    /** Offers a person their own read (live only), and says when one is under way. */
    looking?: "ready" | "busy";
    onread?: () => void;
  }

  let { shown, whose, stale, drawn, display, looking, onread }: Props = $props();

  function box(frame: Rect): Rect {
    return {
      left: drawn.left + (frame.left / display.width) * drawn.width,
      top: drawn.top + (frame.top / display.height) * drawn.height,
      width: (frame.width / display.width) * drawn.width,
      height: (frame.height / display.height) * drawn.height,
    };
  }

  const state = $derived.by(() => {
    switch (shown.kind) {
      case "read": {
        const { query, found } = shown.read;
        const words = query === null ? whose : found.length === 0 ? `${whose} · “${query}” not found` : `${whose} · “${query}” ×${found.length}`;
        return stale ? `${words} · acted since` : words;
      }
      case "loading":
        return "Reading…";
      case "failed":
        return `Can't read the screen: ${shown.why}`;
      case "nothing":
        return "No text read yet";
      default: {
        const never: never = shown;
        return never;
      }
    }
  });
</script>

<div class={["text", { stale }]} role="presentation">
  {#if shown.kind === "read"}
    {#each shown.read.lines as line, index (index)}
      {@const rect = box(line.frame)}
      <div
        class="line"
        style:left="{rect.left}px"
        style:top="{rect.top}px"
        style:width="{rect.width}px"
        style:height="{rect.height}px"
      >
        <span class={["tag", { below: rect.top < 22 }]}>{line.text}</span>
      </div>
    {/each}
    {#each shown.read.found as found, index (index)}
      {@const rect = box(found)}
      <div
        class="found"
        style:left="{rect.left - 2}px"
        style:top="{rect.top - 2}px"
        style:width="{rect.width + 4}px"
        style:height="{rect.height + 4}px"
      ></div>
    {/each}
  {/if}
  <div class="chip" style:top="{drawn.top + 8}px">
    <ScanText size={12} />
    <span class={{ warn: stale && shown.kind === "read" }}>{state}</span>
    {#if looking !== undefined && onread !== undefined}
      <button disabled={looking === "busy"} onclick={onread}>
        {#if looking === "busy"}<LoaderCircle size={11} class="spin" />Reading…{:else}Read now{/if}
      </button>
    {/if}
  </div>
</div>

<style>
  .text {
    position: absolute;
    inset: 0;
    pointer-events: none;
    overflow: hidden;
    --line: #f0b429;
  }
  .line {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid var(--line);
    border-radius: 0;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4);
    opacity: 0.75;
    pointer-events: auto;
  }
  .text.stale .line {
    opacity: 0.35;
  }
  .line:hover {
    opacity: 1;
    background: color-mix(in srgb, var(--line) 15%, transparent);
  }
  .tag {
    display: none;
    position: absolute;
    left: -1px;
    bottom: calc(100% + 3px);
    max-width: 360px;
    padding: 2px 6px;
    border-radius: 0;
    background: rgba(0, 0, 0, 0.8);
    color: #fff;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tag.below {
    top: calc(100% + 3px);
    bottom: auto;
  }
  .line:hover .tag {
    display: block;
  }
  .found {
    position: absolute;
    box-sizing: border-box;
    border: 2px solid var(--accent);
    border-radius: 0;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
  }
  .chip {
    position: absolute;
    right: 8px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 4px 3px 8px;
    border-radius: 0;
    background: rgba(0, 0, 0, 0.7);
    color: #fff;
    font-size: 11px;
    pointer-events: auto;
  }
  .chip .warn {
    color: var(--warning);
  }
  .chip button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-height: 0;
    padding: 1px 6px;
    border: 0;
    border-radius: 0;
    background: rgba(255, 255, 255, 0.16);
    color: #fff;
    font: inherit;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
    cursor: pointer;
  }
  .chip button:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.26);
  }
  .chip button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>

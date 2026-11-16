<script lang="ts">
  import type { Component } from "svelte";
  import {
    ArrowDownUp,
    Bot,
    Hand,
    Highlighter,
    Hourglass,
    Keyboard,
    MousePointer2,
    MousePointerClick,
    Move,
    Power,
    ScanEye,
    Terminal,
    TextCursorInput,
  } from "@lucide/svelte";
  import type { DisplayPoint, Rect, Size } from "./lib/geometry";
  import { hue } from "./lib/hues";
  import { type Attention, pointerOf } from "./lib/attention";
  import { type AgentScene, CALLOUT_FADE_MS, type Call, type Gesture, type Scene, type Shown, arrowEnds, markOpacity, captionSide, describe, swept } from "./lib/overlay";

  interface Props {
    scene: Scene;
    /** Where the picture is drawn in the screen's box, in CSS pixels. */
    drawn: Rect;
    /** The display's size, in points. */
    display: Size;
    /** The page's clock, in the scene's milliseconds. */
    now: number;
    /** What the agent that took the latest snapshot reads of it, if anything. */
    attention?: Attention | null;
  }

  let { scene, drawn, display, now, attention = null }: Props = $props();

  /** A look's band shows only once the look has run this long: a snapshot is read element by element instead, and is quicker. */
  const BAND_AFTER_MS = 300;

  /** How long the agent's pointer stays once it stops acting. */
  const POINTER_MS = 6000;

  function px(point: DisplayPoint): { x: number; y: number } {
    return {
      x: drawn.left + (point.x / display.width) * drawn.width,
      y: drawn.top + (point.y / display.height) * drawn.height,
    };
  }

  function box(frame: Rect): Rect {
    const corner = px({ x: frame.left, y: frame.top });
    return {
      left: corner.x,
      top: corner.y,
      width: (frame.width / display.width) * drawn.width,
      height: (frame.height / display.height) * drawn.height,
    };
  }

  function icon(gesture: Gesture): Component<{ size?: number }> {
    switch (gesture) {
      case "click":
      case "double-click":
      case "secondary-click":
        return MousePointerClick;
      case "move":
        return Move;
      case "drag":
        return Hand;
      case "scroll":
        return ArrowDownUp;
      case "key":
        return Keyboard;
      case "type":
        return TextCursorInput;
      case "act":
        return MousePointer2;
      case "look":
        return ScanEye;
      case "wait":
        return Hourglass;
      case "run":
        return Terminal;
      case "lifecycle":
        return Power;
      case "mark":
        return Highlighter;
      case "other":
        return Bot;
      default: {
        const never: never = gesture;
        return never;
      }
    }
  }

  /** A mark's box, in CSS pixels: an area's, or a point's (no size). */
  function markBox(shown: Shown): Rect {
    const on = shown.callout.on;
    if (on.kind === "area") return box(on.frame);
    const at = px(on.at);
    return { left: at.x, top: at.y, width: 0, height: 0 };
  }

  /** How much of a mark shows, fading in and out. */
  function showing(shown: Shown): number {
    return Math.max(0, Math.min(1, (now - shown.since) / CALLOUT_FADE_MS, (shown.until - now) / CALLOUT_FADE_MS));
  }

  /** What a mark's chip says: its note, else the element's name. */
  function words(shown: Shown): string | null {
    const on = shown.callout.on;
    return shown.callout.note ?? (on.kind === "area" && on.label !== "" ? on.label : null);
  }

  /** Several agents act: each is named by its client. */
  const named = $derived(scene.agents.length > 1);

  /** What an agent's call reads as, named when several act. */
  function saying(agent: AgentScene, call: Call): string {
    return named && agent.client !== "" ? `${agent.client} · ${describe(call)}` : describe(call);
  }

  /** An agent's colours, as the CSS variables its marks and pointer use. */
  function colours(agent: AgentScene): string {
    const { fill, soft, ink } = hue(agent.session);
    return `--agent: ${fill}; --agent-soft: ${soft}; --agent-ink: ${ink}`;
  }

  /** The latest look's band: one at a time, as several mixed read as mud. None over a snapshot being read. */
  const scanning = $derived.by(() => {
    const latest = scene.agents.reduce<AgentScene | null>(
      (latest, agent) => (agent.scan !== null && (latest?.scan ?? -Infinity) < agent.scan ? agent : latest),
      null,
    );
    if (latest === null || latest.scan === null || now - latest.scan < BAND_AFTER_MS) return null;
    return attention !== null && attention.session === latest.session && attention.since >= latest.scan ? null : latest;
  });

  /** The agent reading, if it is one of the scene's. */
  const reader = $derived(attention === null ? null : (scene.agents.find((agent) => agent.session === attention.session) ?? null));

  /** Its work off the screen under way: a command, a file read or written. */
  function running(agent: AgentScene): Call | null {
    const call = agent.current?.call ?? null;
    return call !== null && call.gesture === "run" ? call : null;
  }
</script>

<div class="overlay" aria-hidden="true">
  {#if scanning !== null && scanning.scan !== null}
    <div class="scan" style={colours(scanning)} style:left="{drawn.left}px" style:top="{drawn.top}px" style:width="{drawn.width}px" style:height="{drawn.height}px">
      <span class="band" style:top="{-30 + swept(scanning.scan, now) * 130}%"></span>
    </div>
  {/if}
  {#if attention !== null && reader !== null && attention.lit.length > 0}
    <div class="reading" style={colours(reader)} style:opacity={attention.presence}>
      {#each attention.lit as { glance, glow } (glance.index)}
        {@const rect = box(glance.frame)}
        {@const head = attention.head?.index === glance.index}
        <div
          class={["glance", { head }]}
          style:left="{rect.left}px"
          style:top="{rect.top}px"
          style:width="{rect.width}px"
          style:height="{rect.height}px"
          style:--glow={glow}
        ></div>
      {/each}
    </div>
  {/if}
  {#each scene.agents as agent (agent.session)}
    <div class="agent" style={colours(agent)}>
  {#each agent.marks as mark (mark.id)}
    {@const fading = markOpacity(mark, now)}
    {#if mark.kind === "ripple"}
      {@const at = px(mark.at)}
      <span class={["ripple", mark.gesture]} style:left="{at.x}px" style:top="{at.y}px"></span>
      {#if mark.gesture === "double-click"}
        <span class="ripple second" style:left="{at.x}px" style:top="{at.y}px"></span>
      {/if}
    {:else if mark.kind === "box"}
      {@const rect = box(mark.frame)}
      <div class="box" style:left="{rect.left}px" style:top="{rect.top}px" style:width="{rect.width}px" style:height="{rect.height}px" style:opacity={fading}>
        {#if mark.label !== ""}<span class="label">{mark.label}</span>{/if}
      </div>
    {:else if mark.kind === "path"}
      {@const from = px(mark.from)}
      {@const to = px(mark.to)}
      <svg class="path" style:opacity={fading}>
        <line x1={from.x} y1={from.y} x2={to.x} y2={to.y} />
        <circle cx={from.x} cy={from.y} r="4" />
        <circle cx={to.x} cy={to.y} r="6" class="end" />
      </svg>
    {:else}
      {@const at = px(mark.at)}
      <span class="ripple failed" style:left="{at.x}px" style:top="{at.y}px"></span>
      <span class="reason" style:left="{at.x}px" style:top="{at.y}px" style:opacity={fading}>{mark.reason}</span>
    {/if}
  {/each}

    </div>
  {/each}

  {#each scene.callouts as mark (mark.id)}
    {@const on = markBox(mark)}
    {@const point = on.width === 0 && on.height === 0}
    {@const below = on.top + on.height + 40 <= drawn.top + drawn.height}
    {@const said = words(mark)}
    <div class="callout" style:opacity={showing(mark)}>
      {#if mark.callout.shape.kind === "box"}
        {@const pad = point ? 18 : 3}
        <div
          class={["frame", { point }]}
          style:left="{on.left - pad}px"
          style:top="{on.top - pad}px"
          style:width="{on.width + pad * 2}px"
          style:height="{on.height + pad * 2}px"
        ></div>
        {#if said !== null}
          <span class="note" style:left="{on.left - pad}px" style:top="{below ? on.top + on.height + pad + 10 : on.top - pad - 34}px"
            ><Highlighter size={13} /><span>{said}</span></span
          >
        {/if}
      {:else if mark.callout.shape.kind === "dot"}
        {@const x = on.left + on.width / 2}
        {@const y = on.top + on.height / 2}
        <span class="pulse" style:left="{x}px" style:top="{y}px"></span>
        <span class="dot" style:left="{x}px" style:top="{y}px"></span>
        {#if said !== null}
          <span class="note" style:left="{x - 7}px" style:top="{y + 21}px"><Highlighter size={13} /><span>{said}</span></span>
        {/if}
      {:else}
        {@const from = mark.callout.shape.from}
        {@const ends = arrowEnds(on, from === null ? null : px(from), drawn)}
        {@const length = Math.max(1, Math.hypot(ends.tip.x - ends.tail.x, ends.tip.y - ends.tail.y))}
        {@const ux = (ends.tip.x - ends.tail.x) / length}
        {@const uy = (ends.tip.y - ends.tail.y) / length}
        {@const base = { x: ends.tip.x - ux * 14, y: ends.tip.y - uy * 14 }}
        <svg class="pointer">
          <line class="halo" x1={ends.tail.x} y1={ends.tail.y} x2={base.x} y2={base.y} />
          <line x1={ends.tail.x} y1={ends.tail.y} x2={base.x} y2={base.y} />
          <polygon
            points="{ends.tip.x},{ends.tip.y} {base.x - uy * 8.4},{base.y + ux * 8.4} {base.x + uy * 8.4},{base.y - ux * 8.4}"
          />
        </svg>
        {#if said !== null}
          <span class="note" style:left="{ends.tail.x - 8}px" style:top="{ends.tail.y > ends.tip.y ? ends.tail.y + 8 : ends.tail.y - 34}px"
            ><Highlighter size={13} /><span>{said}</span></span
          >
        {/if}
      {/if}
    </div>
  {/each}

  {#each scene.agents as agent (agent.session)}
    {@const run = running(agent)}
    {#if run !== null}
      <span class="run" style={colours(agent)} style:left="{drawn.left + 12}px" style:top="{drawn.top + drawn.height - 40}px">
        <Terminal size={13} /><span>{named && agent.client !== "" ? `${agent.client} · ` : ""}{run.caption === "" ? describe(run) : run.caption}{run.why === null ? "" : ` · ${run.why}`}</span><span class="spinner"></span>
      </span>
    {/if}
  {/each}

  {#each scene.agents as agent (agent.session)}
    {@const pointer = pointerOf(agent, attention)}
    {@const reads = attention !== null && attention.session === agent.session && attention.presence > 0}
    {#if pointer !== null}
      {@const at = px(pointer)}
      {@const current = agent.current?.call ?? null}
      {@const CurrentIcon = current === null ? null : icon(current.gesture)}
      {@const side = captionSide(at.x, drawn.left + drawn.width)}
      {@const head = reads && current === null ? (attention?.head ?? null) : null}
      <div
        class={["cursor", { shown: agent.current !== null || reads || now - agent.active < POINTER_MS, reading: head !== null }]}
        style={colours(agent)}
        style:transform="translate({at.x}px, {at.y}px)"
      >
        <svg class="arrow" width="22" height="22" viewBox="0 0 24 24">
          <path d="M4 2.5 L4 19 L8.6 14.8 L11.6 21.4 L14.6 20 L11.7 13.6 L18 13.6 Z" />
        </svg>
        {#if head !== null}
          <span class={["caption", "read", side]}><ScanEye size={13} /><span>{head.label}</span></span>
        {:else if current !== null && CurrentIcon !== null && running(agent) === null}
          <span class={["caption", side]}>
            <CurrentIcon size={13} />
            <span>{saying(agent, current)}</span>
          </span>
          {#if current.why !== null}<span class={["why", side]}>{current.why}</span>{/if}
        {:else if named && agent.client !== ""}
          <span class={["caption", side]}><Bot size={13} /><span>{agent.client}</span></span>
        {:else}
          <span class={["caption", "idle", side]}><Bot size={13} /></span>
        {/if}
      </div>
    {/if}
  {/each}

</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    pointer-events: none;
    overflow: hidden;
    --mark: #ff5ca8;
    --mark-soft: rgba(255, 92, 168, 0.18);
  }

  .agent {
    position: absolute;
    inset: 0;
  }

  /* What an agent points out to the people watching. */
  .callout {
    position: absolute;
    inset: 0;
  }
  .frame {
    position: absolute;
    border: 2px solid var(--mark);
    background: var(--mark-soft);
    animation: settle 0.25s ease-out;
  }
  .frame.point {
    border-radius: 50%;
  }
  .dot {
    position: absolute;
    width: 14px;
    height: 14px;
    margin: -7px 0 0 -7px;
    background: var(--mark);
    border: 3px solid #fff;
    box-sizing: border-box;
  }
  .pulse {
    position: absolute;
    width: 44px;
    height: 44px;
    margin: -22px 0 0 -22px;
    border-radius: 50%;
    border: 2.5px solid var(--mark);
    background: var(--mark-soft);
    animation: ripple 1.4s cubic-bezier(0.2, 0.7, 0.3, 1) infinite;
  }
  .pointer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }
  .pointer line {
    stroke: var(--mark);
    stroke-width: 4;
    stroke-linecap: round;
  }
  .pointer line.halo {
    stroke: #fff;
    stroke-width: 7;
  }
  .pointer polygon {
    fill: var(--mark);
  }
  .note {
    position: absolute;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px 4px 6px;
    font: 500 11px/1.2 var(--mono);
    letter-spacing: 0.03em;
    white-space: nowrap;
    max-width: 320px;
    overflow: hidden;
    color: #fff;
    background: color-mix(in srgb, var(--mark) 75%, #000);
  }
  .note span {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .cursor {
    position: absolute;
    left: 0;
    top: 0;
    opacity: 0;
    transition:
      transform 0.45s cubic-bezier(0.3, 0.9, 0.3, 1),
      opacity 0.4s ease;
    will-change: transform;
  }
  .cursor.shown {
    opacity: 1;
  }
  /* Reading, its eyes jump from element to element: the pointer keeps up. */
  .cursor.reading {
    transition:
      transform 0.1s ease-out,
      opacity 0.4s ease;
  }
  .arrow {
    position: absolute;
    left: -4px;
    top: -2px;
  }
  .arrow path {
    fill: var(--agent);
    stroke: #fff;
    stroke-width: 1.5;
    stroke-linejoin: miter;
  }
  .caption,
  .reason {
    position: absolute;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px 4px 6px;
    font: 500 11px/1.2 var(--mono);
    letter-spacing: 0.03em;
    text-transform: uppercase;
    white-space: nowrap;
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: #fff;
    background: var(--agent-ink);
  }
  .caption {
    left: 18px;
    top: 20px;
    animation: pop 0.2s ease-out;
  }
  .caption span {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* Near the right edge: the caption ends just left of the pointer. */
  .caption.left {
    left: auto;
    right: 6px;
  }
  .caption.idle {
    padding: 4px;
  }
  /* Why the agent does it, in its own words, under what it does. */
  .why {
    position: absolute;
    left: 18px;
    top: 44px;
    padding: 4px 8px;
    font: 500 11px/1.2 var(--mono);
    white-space: nowrap;
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--agent-ink);
    background: var(--agent);
    animation: pop 0.2s ease-out;
  }
  .why.left {
    left: auto;
    right: 6px;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.85);
    }
  }
  .ripple {
    position: absolute;
    width: 44px;
    height: 44px;
    margin: -22px 0 0 -22px;
    border-radius: 50%;
    border: 2.5px solid var(--agent);
    background: var(--agent-soft);
    animation: ripple 0.9s cubic-bezier(0.2, 0.7, 0.3, 1) forwards;
  }
  .ripple.second {
    animation-delay: 0.14s;
    opacity: 0;
  }
  .ripple.secondary-click {
    border-style: dashed;
  }
  .ripple.scroll {
    border-radius: 0;
  }
  .ripple.failed {
    border-color: #ff6b6b;
    background: rgba(255, 107, 107, 0.2);
  }
  @keyframes ripple {
    0% {
      opacity: 1;
      transform: scale(0.2);
    }
    100% {
      opacity: 0;
      transform: scale(1.25);
    }
  }

  .box {
    position: absolute;
    border: 2px solid var(--agent);
    background: var(--agent-soft);
    animation: settle 0.25s ease-out;
  }
  @keyframes settle {
    from {
      transform: scale(1.12);
      opacity: 0;
    }
  }
  .label {
    position: absolute;
    left: -2px;
    bottom: calc(100% + 5px);
    padding: 2px 6px;
    font: 500 10.5px/1.4 var(--mono);
    color: #fff;
    background: var(--agent);
    white-space: nowrap;
  }

  .path {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }
  .path line {
    stroke: var(--agent);
    stroke-width: 3;
    stroke-linecap: butt;
    stroke-dasharray: 3 5;
    animation: march 0.6s linear infinite;
  }
  .path circle {
    fill: var(--agent);
  }
  .path .end {
    fill: #fff;
    stroke: var(--agent);
    stroke-width: 3;
  }
  @keyframes march {
    to {
      stroke-dashoffset: -12;
    }
  }

  .reason {
    margin: 26px 0 0 -8px;
    background: #c93a3a;
  }

  /* The agent reads a snapshot: each element it was shown lights up in turn, then settles. */
  .reading {
    position: absolute;
    inset: 0;
  }
  .glance {
    position: absolute;
    box-sizing: border-box;
    border: 1.5px solid color-mix(in srgb, var(--agent) calc(var(--glow) * 100%), transparent);
    background: color-mix(in srgb, var(--agent) calc(var(--glow) * 22%), transparent);
    animation: settle 0.18s ease-out;
  }
  .glance.head {
    border-width: 2.5px;
    box-shadow:
      0 0 0 1px rgba(0, 0, 0, 0.35),
      0 0 18px color-mix(in srgb, var(--agent) 60%, transparent);
  }
  .caption.read {
    text-transform: none;
    letter-spacing: 0;
  }

  /* Work off the screen under way: a command run, a file read or written. */
  .run {
    position: absolute;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 9px 5px 7px;
    font: 500 11px/1.2 var(--mono);
    letter-spacing: 0.02em;
    white-space: nowrap;
    max-width: 360px;
    overflow: hidden;
    color: #fff;
    background: var(--agent-ink);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--agent) 60%, transparent);
    animation: pop 0.2s ease-out;
  }
  .run span:not(.spinner) {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .spinner {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--agent);
    animation: blink 0.9s ease-in-out infinite alternate;
  }
  @keyframes blink {
    to {
      opacity: 0.25;
    }
  }

  /* The agent reads the screen: a soft band sweeps down it, each pass to the end. */
  .scan {
    position: absolute;
    overflow: hidden;
    border-radius: 2px;
  }
  .band {
    position: absolute;
    left: 0;
    right: 0;
    height: 30%;
    background: linear-gradient(
      to bottom,
      transparent,
      color-mix(in srgb, var(--agent) 16%, transparent) 70%,
      color-mix(in srgb, var(--agent) 55%, transparent) 99%,
      transparent
    );
  }
</style>

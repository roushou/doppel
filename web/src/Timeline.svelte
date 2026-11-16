<script lang="ts">
  import { describe } from "./lib/overlay";
  import { hue } from "./lib/hues";
  import { type Moment, family, lanes, within } from "./lib/timeline";

  interface Props {
    moments: readonly Moment[];
    /** The stretch shown, in the moments' milliseconds. */
    from: number;
    to: number;
    /** The playhead, if there is one. */
    at?: number | null;
    /** The clock: where a call under way has got to. */
    now: number;
    /** How a time on the axis reads. */
    label: (ms: number) => string;
    /** Called with the time under a click; without it the strip can't be clicked. */
    onseek?: (ms: number) => void;
  }

  let { moments, from, to, at = null, now, label, onseek }: Props = $props();

  let width = $state(0);
  const span = $derived(Math.max(1, to - from));
  const shown = $derived(within(moments, from, to, now));
  /** A lane per session, named, when more than one acted in the stretch shown. */
  const rows = $derived(lanes(shown));
  /** Five ticks along the axis. */
  const ticks = $derived([0, 0.25, 0.5, 0.75, 1].map((f) => from + f * span));

  function x(ms: number): number {
    return ((ms - from) / span) * 100;
  }

  function title(moment: Moment): string {
    const took = ((moment.endMs ?? now) - moment.startMs) / 1000;
    const what = moment.person ? "A person had control" : describe(moment.call);
    const caption = moment.call.caption !== "" && moment.call.caption !== what ? ` (${moment.call.caption})` : "";
    const ending = moment.endMs === null ? " · under way" : moment.failed !== null ? ` · ${moment.failed}` : "";
    return `${label(moment.startMs)} ${what}${caption} · ${took.toFixed(1)} s${ending}`;
  }

  function seek(event: MouseEvent): void {
    if (onseek === undefined || width === 0) return;
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    onseek(from + ((event.clientX - box.left) / box.width) * span);
  }
</script>

{#snippet lane(laneMoments: Moment[])}
  <!-- Seeking by keyboard is the video's own controls' job; the strip is a shortcut for the pointer. -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="lane" onclick={seek}>
    {#each laneMoments as moment (moment.key)}
      {@const left = Math.max(0, x(moment.startMs))}
      {@const right = Math.min(100, x(moment.endMs ?? now))}
      <span
        class={[
          "moment",
          moment.person ? "person" : family(moment.call.gesture),
          { failed: moment.failed !== null, open: moment.endMs === null },
        ]}
        style:--agent={hue(moment.call.session).fill}
        style:left="{left}%"
        style:width="max(4px, {right - left}%)"
        title={title(moment)}
      ></span>
    {/each}
    {#if at !== null && at >= from && at <= to}
      <span class="playhead" style:left="{x(at)}%"></span>
    {/if}
  </div>
{/snippet}

<div class={["timeline", { seekable: onseek !== undefined, lanes: rows.length > 1 }]} bind:clientWidth={width}>
  <div class="box">
    {#if rows.length > 1}
      {#each rows as row (row.session)}
        <div class="row">
          <span class="who" title={row.client}><span class="dot" style:background={hue(row.session).fill}></span>{row.client === "" ? `session ${row.session}` : row.client}</span>
          {@render lane(row.moments)}
        </div>
      {/each}
    {:else}
      <div class="single">{@render lane(shown)}</div>
    {/if}
  </div>
  <div class="axis" aria-hidden="true">
    {#each ticks as tick, index (index)}
      <span style:left="{x(tick)}%" class={{ first: index === 0, last: index === ticks.length - 1 }}>{label(tick)}</span>
    {/each}
  </div>
</div>

<style>
  /* The tape: each call a run of dots in its family's colour, on hairline lanes. */
  .timeline {
    position: relative;
    padding: 2px 0 0;
    user-select: none;
  }
  .seekable {
    cursor: pointer;
  }
  /* Room for two lanes whoever acts, so the screen above never moves; a third agent's and on scroll into view. */
  .box {
    display: flex;
    flex-direction: column;
    height: 46px;
    overflow-y: auto;
  }
  .single {
    margin: auto 0;
  }
  .row {
    flex: none;
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 8px;
    align-items: center;
    border-bottom: 1px solid var(--line);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font: 500 10.5px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dot {
    flex: none;
    width: 9px;
    height: 9px;
  }
  .lanes .axis {
    margin-left: 118px;
  }
  .lane {
    position: relative;
    height: 22px;
  }
  .timeline:not(.lanes) .lane {
    border-bottom: 1px solid var(--line);
  }
  /* A run takes its agent's colour; looking and waiting are drawn faint, so what changed the screen stands out. */
  .moment {
    position: absolute;
    top: 9px;
    height: 4px;
    --c: var(--agent);
    background: repeating-linear-gradient(90deg, var(--c) 0 4px, transparent 4px 6px);
  }
  .moment:hover {
    outline: 1px solid var(--text);
    outline-offset: 3px;
  }
  .look,
  .wait {
    --c: var(--faint);
  }
  .mark {
    --c: #ff5ca8;
  }
  .person {
    top: 7px;
    height: 8px;
    background: repeating-linear-gradient(135deg, var(--text) 0 2px, transparent 2px 5px);
  }
  .failed {
    --c: var(--danger);
    outline: 1px solid var(--danger);
    outline-offset: 2px;
  }
  .open {
    animation: pulse 1.2s steps(2, jump-none) infinite;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--text);
  }
  .playhead::before {
    content: "";
    position: absolute;
    left: -4px;
    top: 0;
    width: 9px;
    height: 9px;
    background: var(--text);
  }
  .axis {
    position: relative;
    height: 18px;
    font: 400 10.5px/1 var(--mono);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .axis span {
    position: absolute;
    top: 4px;
    transform: translateX(-50%);
    white-space: nowrap;
  }
  .axis .first {
    transform: none;
  }
  .axis .last {
    transform: translateX(-100%);
  }
</style>

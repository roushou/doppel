<script lang="ts">
  import { onMount } from "svelte";
  import { create } from "@bufbuild/protobuf";
  import { ListRecordingsSchema, OpenRecordingSchema, OpenSeenSnapshotSchema } from "./gen/doppel/daemon/v1/daemon_pb";
  import Replay from "./Replay.svelte";
  import { type Operation, type Outcome, refusal } from "./lib/connection";
  import {
    parseRecording,
    parseRecordingList,
    type Recording,
    type RecordingId,
    type RecordingSummary,
  } from "./lib/recordings";
  import { type Seen, readSnapshot } from "./lib/tree";
  import { ArrowLeft, Film, VideoOff } from "@lucide/svelte";
  import { byDay, type Day } from "./lib/library";
  import { hue } from "./lib/hues";

  interface Props {
    /** Sends a request on the page's connection. */
    request: (operation: Operation) => Promise<Outcome | undefined>;
    /** The machine whose recordings to list first; `null` for all. */
    machine?: string | null;
  }

  let { request, machine = null }: Props = $props();
  // svelte-ignore state_referenced_locally
  let only: string | null = $state(machine);

  type View =
    | { kind: "loading" }
    | { kind: "list"; recordings: RecordingSummary[] }
    | { kind: "opening"; id: RecordingId }
    | { kind: "replay"; recording: Recording }
    | { kind: "failed"; why: string };

  let view: View = $state({ kind: "loading" });

  async function load(): Promise<void> {
    view = { kind: "loading" };
    const outcome = await request({ case: "listRecordings", value: create(ListRecordingsSchema) });
    view =
      outcome?.case === "recordingList"
        ? { kind: "list", recordings: parseRecordingList(outcome.value) }
        : { kind: "failed", why: outcome === undefined ? "not connected" : refusal(outcome) };
  }

  async function open(id: RecordingId): Promise<void> {
    view = { kind: "opening", id };
    const outcome = await request({ case: "openRecording", value: create(OpenRecordingSchema, { id }) });
    view =
      outcome?.case === "recording"
        ? { kind: "replay", recording: parseRecording(outcome.value) }
        : { kind: "failed", why: outcome === undefined ? "not connected" : refusal(outcome) };
  }

  /** The snapshot a recording's trace line kept, or why it can't be shown. */
  async function seen(
    id: RecordingId,
    line: number,
  ): Promise<{ kind: "seen"; seen: Seen } | { kind: "failed"; why: string }> {
    const outcome = await request({
      case: "openSeenSnapshot",
      value: create(OpenSeenSnapshotSchema, { recording: id, line }),
    });
    if (outcome?.case === "axSnapshot") return { kind: "seen", seen: readSnapshot(outcome.value) };
    return { kind: "failed", why: outcome === undefined ? "not connected" : refusal(outcome) };
  }

  function length(ms: number): string {
    const seconds = Math.round(ms / 1000);
    return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, "0")}s`;
  }

  function dayName(day: Day): string {
    switch (day.kind) {
      case "today":
        return "Today";
      case "yesterday":
        return "Yesterday";
      case "on":
        return day.date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
      default: {
        const never: never = day;
        return never;
      }
    }
  }

  function time(date: Date): string {
    return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  }

  onMount(() => void load());
</script>

{#if view.kind === "loading"}
  <p class="muted">Loading recordings…</p>
{:else if view.kind === "opening"}
  <p class="muted">Opening {view.id}…</p>
{:else if view.kind === "failed"}
  <p class="notice" role="status">Recordings: {view.why}</p>
  <button onclick={() => void load()}>Try again</button>
{:else if view.kind === "replay"}
  <div class="bar">
    <button class="quiet" onclick={() => void load()}><ArrowLeft size={14} />Recordings</button>
    <h2>{view.recording.id}</h2>
  </div>
  {@const id = view.recording.id}
  <Replay recording={view.recording} loadSeen={(line) => seen(id, line)} />
{:else if view.recordings.length === 0}
  <p class="muted">
    No recordings. An agent's session is recorded with <code>doppel mcp --record</code>.
  </p>
{:else}
  {@const machines = [...new Set(view.recordings.flatMap((summary) => summary.machines))].sort()}
  {@const shown = view.recordings.filter((summary) => only === null || summary.machines.includes(only))}
  {#if machines.length > 1}
    <div class="filter" role="radiogroup" aria-label="Machine">
      <button role="radio" aria-checked={only === null} class={{ on: only === null }} onclick={() => (only = null)}>All</button>
      {#each machines as name (name)}
        <button role="radio" aria-checked={only === name} class={{ on: only === name }} onclick={() => (only = name)}>{name}</button>
      {/each}
    </div>
  {/if}
  {@const longest = Math.max(1, ...shown.map((summary) => summary.tracedMs))}
  {#each byDay(shown, new Date()) as group (group.key)}
    <section class="day">
      <h2 class="label-mono">{dayName(group.day)}<span class="count">{group.recordings.length}</span></h2>
      <ul class="recordings">
        {#each group.recordings as summary (summary.id)}
          <li>
            <button class="recording" onclick={() => void open(summary.id)} title={summary.id}>
              <span class="time">{time(summary.started)}</span>
              <span class="swatch" style:background={hue(Number(summary.session)).fill} aria-hidden="true"></span>
              <span class="machines">{summary.machines.length === 0 ? "—" : summary.machines.join(", ")}</span>
              <span class="span" aria-hidden="true"><span style:width={`${(summary.tracedMs / longest) * 100}%`}></span></span>
              <span class="length">{length(summary.tracedMs)}</span>
              <span class="calls">{summary.calls}<span class="unit">{summary.calls === 1 ? "call" : "calls"}</span></span>
              {#if summary.videos === 0}
                <VideoOff size={14} class="novideo" aria-label="No video" />
              {:else}
                <Film size={14} aria-label={`${summary.videos} ${summary.videos === 1 ? "video" : "videos"}`} />
              {/if}
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {:else}
    <p class="muted">No recordings of {only}.</p>
  {/each}
{/if}

<style>
  .bar {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin-bottom: 12px;
  }
  .bar h2 {
    font-size: 15px;
    margin: 0;
    overflow-wrap: anywhere;
  }
  .day + .day {
    margin-top: 18px;
  }
  .day h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 6px;
    font-size: 11px;
    color: var(--muted);
  }
  .count {
    color: var(--faint);
  }
  .recordings {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
  }
  .recording {
    display: grid;
    grid-template-columns: 62px 10px minmax(80px, 1fr) minmax(60px, 2fr) 56px 64px 16px;
    gap: 12px;
    align-items: center;
    width: 100%;
    height: 36px;
    padding: 0 12px;
    text-align: left;
    border: 0;
    border-bottom: 1px solid var(--line);
    border-radius: 0;
    background: transparent;
    color: var(--ink);
    font: inherit;
    text-transform: none;
    letter-spacing: 0;
    font-size: 13px;
    cursor: pointer;
  }
  .recording:hover {
    background: var(--panel);
  }
  .recording:hover .span > span {
    background: var(--live);
  }
  .time,
  .length,
  .calls {
    font: 500 12px/1 var(--mono);
    font-variant-numeric: tabular-nums;
  }
  .time {
    color: var(--strong);
  }
  .swatch {
    width: 10px;
    height: 10px;
  }
  .machines {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .span {
    height: 4px;
    background: var(--line);
  }
  .span > span {
    display: block;
    height: 100%;
    min-width: 2px;
    background: var(--muted);
  }
  .length,
  .calls {
    text-align: right;
    color: var(--muted);
  }
  .unit {
    margin-left: 4px;
    color: var(--faint);
  }
  .recording :global(.novideo) {
    color: var(--faint);
  }
  @media (max-width: 560px) {
    .recording {
      grid-template-columns: 62px 10px minmax(0, 1fr) 48px 16px;
    }
    .span,
    .calls {
      display: none;
    }
  }
  .filter {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-bottom: 12px;
  }
  .filter button {
    height: 26px;
    border-radius: 0;
    font-size: 12px;
    background: transparent;
    color: var(--muted);
  }
  .filter button.on {
    background: var(--slab);
    border-color: transparent;
    color: var(--on-slab);
    box-shadow: inset 0 -2px 0 var(--live);
  }
  .muted {
    color: var(--muted);
  }
  .notice {
    padding: 8px 12px;
    border-radius: 0;
    background: var(--notice);
  }
</style>

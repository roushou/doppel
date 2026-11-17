<script lang="ts">
  import {
    aligned,
    type Call,
    callsAt,
    type Recording,
    seekAt,
    seekTo,
    sessionMs,
    type TraceLine,
    videoUrl,
  } from "./lib/recordings";
  import { onDestroy, onMount } from "svelte";
  import type { Film, Films } from "./lib/film";
  import { contain } from "./lib/geometry";
  import { ListTree, Pause, Play, ScanText } from "@lucide/svelte";
  import { follow as place, Transport } from "./lib/transport";
  import { attentionAt } from "./lib/attention";
  import { fromTrace, readAt, seenAt } from "./lib/timeline";
  import Text from "./Text.svelte";
  import { type Seen, refsOf } from "./lib/tree";
  import Elements from "./Elements.svelte";
  import Overlay from "./Overlay.svelte";
  import Tree from "./Tree.svelte";
  import Timeline from "./Timeline.svelte";

  interface Props {
    recording: Recording;
    /** The recording as the daemon sent it (`doppel.daemon.v1.Recording`), for the film. */
    wire: Uint8Array;
    /** The film's module, which draws what agents did over the video; none if it didn't load. */
    films: Promise<Films | null>;
    /** The snapshot a trace line kept, with its `doppel.agent.v1.AxSnapshot` bytes, or why it can't be shown. */
    loadSeen: (line: number) => Promise<{ kind: "seen"; seen: Seen; wire: Uint8Array } | { kind: "failed"; why: string }>;
  }

  let { recording, wire, films, loadSeen }: Props = $props();

  /** Which of the recording's videos is showing. */
  let current = $state(0);
  /** Whose screen is shown, when the session had several machines. */
  let machine: string | null = $state(null);
  let player: HTMLVideoElement | undefined = $state();
  let list: HTMLOListElement | undefined = $state();
  const rows: HTMLLIElement[] = $state([]);
  /** The session's time at the playhead, when the video is lined up. */
  let now: number | null = $state(null);
  /** The session's end as far as the videos' lengths are known, past the trace's. */
  let filmed = $state(0);
  /** Where to seek once the next video has loaded. */
  let pending: { seconds: number } | null = null;

  const playing = $derived(recording.videos[current]);
  const lined = $derived(aligned(recording));
  const names = $derived([...new Set(recording.videos.map((video) => video.machine))]);
  const machines = $derived(names.length);
  const shownMachine = $derived(machine ?? names[0] ?? null);
  const active = $derived(playing === undefined || now === null ? [] : callsAt(recording.trace, playing.machine, now));
  const calls = $derived(recording.trace.filter((line): line is Call => line.kind === "call"));
  /** The playing machine's calls, on the same timeline a live machine has. */
  const moments = $derived(playing === undefined ? [] : fromTrace(calls, playing.machine));
  /** The session's length as traced, for the timeline's span. */
  const length = $derived(Math.max(1000, filmed, ...calls.map((call) => call.endMs)));
  /** The replay's clock: the video follows it, so the trace plays and seeks to its end. */
  let transport: Transport = $state(Transport.start(1000));
  $effect(() => {
    transport = Transport.lengthen(transport, length, performance.now());
  });
  /** Before the shown machine's first video: nothing of its screen to show yet. */
  let unfilmed = $state(false);
  let boxWidth = $state(0);
  let boxHeight = $state(0);
  /** Where the video is drawn in its box: the overlay's frame, when its display's size was kept. */
  const drawn = $derived(
    playing?.points == null ? null : contain({ left: 0, top: 0, width: boxWidth, height: boxHeight }, playing.points),
  );
  /** The film's module, once loaded. */
  let made: Films | null = $state.raw(null);
  onMount(() => {
    void films.then((loaded) => (made = loaded));
  });
  /** The playing machine's film. */
  let film: Film | null = $state.raw(null);
  /** The trace lines whose snapshots the film has. */
  let given = new Set<number>();
  /** Counts what the film has been given, so a paused frame is drawn again with it. */
  let gave = $state(0);
  $effect(() => {
    const shown = playing?.machine;
    if (made === null || shown === undefined) return;
    let replayed: Film;
    try {
      replayed = made.replay(wire, shown);
    } catch (error) {
      console.warn("the overlay can't replay this recording:", error);
      return;
    }
    film = replayed;
    given = new Set();
    return () => {
      replayed.drop();
      film = null;
    };
  });
  /** Whether the elements the agent last saw by the playhead are shown: the tree, and their boxes over the video. */
  let inspecting = $state(false);
  const paused = $derived(transport.kind !== "playing");
  let wrappers: "show" | "hide" = $state("hide");
  /** Whether the text the agent last read by the playhead is drawn. */
  let texting = $state(false);
  let pointed: number | null = $state(null);
  let picked: number | null = $state(null);
  /** A trace line's snapshot, as far as it has been fetched. */
  type Fetch = { kind: "loading" } | { kind: "seen"; seen: Seen; wire: Uint8Array } | { kind: "failed"; why: string };
  /** Snapshots fetched, by trace line: each is asked for once. */
  let fetched: Map<number, Fetch> = $state(new Map());
  const found = $derived(now === null ? null : seenAt(moments, now));
  const line = $derived(found?.source.kind === "recorded" ? found.source.line : null);
  const fetch = $derived(line === null ? undefined : fetched.get(line));
  const seen = $derived(fetch?.kind === "seen" ? fetch.seen : null);
  const refs = $derived(seen === null ? null : refsOf(seen, found?.moment.refs ?? null));
  /** What the agent reads of the snapshot by the playhead. */
  const attention = $derived(found === null || seen === null || now === null ? null : attentionAt(found, seen, refs, moments, now));
  const empty = $derived(
    fetch === undefined || line === null
      ? line === null
        ? { kind: "none" as const }
        : { kind: "loading" as const }
      : fetch.kind === "failed"
        ? { kind: "failed" as const, why: fetch.why }
        : { kind: "loading" as const },
  );
  $effect(() => {
    // Fetched whether or not the tree shows: the overlay draws what the agent reads of it.
    if (line === null || fetched.has(line)) return;
    const asked = line;
    fetched = new Map(fetched).set(asked, { kind: "loading" });
    void loadSeen(asked).then((answer) => {
      fetched = new Map(fetched).set(asked, answer);
    });
  });
  $effect(() => {
    // Each snapshot fetched goes into the film, so it shows what the agent took in.
    if (film === null) return;
    for (const [at, answer] of fetched) {
      if (answer.kind !== "seen" || given.has(at)) continue;
      given.add(at);
      try {
        film.seen(at, answer.wire);
        gave += 1;
      } catch (error) {
        console.warn(`the overlay can't read the snapshot of line ${at}:`, error);
      }
    }
  });
  $effect(() => {
    void line;
    pointed = null;
    picked = null;
  });

  function ago(ms: number): string {
    return `${(Math.max(0, ms) / 1000).toFixed(1)} s before`;
  }

  /** The clock is read every frame while it plays, so the overlay moves smoothly. */
  let frame: number | null = null;

  /** Puts the video where the clock is: playing along inside it, its last frame past its end. */
  function sync(): void {
    if (!lined) return;
    const wall = performance.now();
    transport = Transport.settle(transport, wall);
    const at = Transport.at(transport, wall);
    now = at;
    const seek = seekAt(recording.videos, shownMachine, at);
    unfilmed = seek === null;
    if (seek === null) {
      player?.pause();
      return;
    }
    if (seek.video !== current) {
      show(seek.video, seek.seconds);
      return;
    }
    if (player === undefined || pending !== null) return;
    const spot = place(seek.seconds, player.duration);
    const along = transport.kind === "playing" && spot.kind === "inside";
    if (along) {
      if (player.paused) void player.play().catch(() => {});
      if (Math.abs(player.currentTime - spot.seconds) > 0.3) player.currentTime = spot.seconds;
    } else {
      if (!player.paused) player.pause();
      if (Math.abs(player.currentTime - spot.seconds) > 0.04) player.currentTime = spot.seconds;
    }
  }

  function run(): void {
    sync();
    frame = transport.kind === "playing" ? requestAnimationFrame(run) : null;
  }

  function toggle(): void {
    const wall = performance.now();
    transport = transport.kind === "playing" ? Transport.pause(transport, wall) : Transport.play(transport, wall);
    if (frame === null) run();
  }

  /** Seeks the shown machine's screen to the session's time `ms`. */
  function seekTime(ms: number): void {
    transport = Transport.seek(transport, ms, performance.now());
    if (frame === null) run();
  }

  function nudge(event: KeyboardEvent): void {
    const step = event.shiftKey ? 1000 : 5000;
    if (event.key === "ArrowLeft") seekTime((now ?? 0) - step);
    else if (event.key === "ArrowRight") seekTime((now ?? 0) + step);
    else return;
    event.preventDefault();
  }

  onDestroy(() => {
    if (frame !== null) cancelAnimationFrame(frame);
  });

  function loaded(): void {
    if (player !== undefined && playing !== undefined && playing.startMs !== null && Number.isFinite(player.duration)) {
      filmed = Math.max(filmed, playing.startMs + player.duration * 1000);
    }
    if (player !== undefined && pending !== null) {
      player.currentTime = pending.seconds;
      pending = null;
    }
    sync();
  }

  function show(index: number, seconds: number): void {
    if (index === current && player !== undefined) {
      player.currentTime = seconds;
      return;
    }
    pending = { seconds };
    current = index;
  }

  function choose(call: Call): void {
    if (call.machine !== null && names.includes(call.machine)) machine = call.machine;
    seekTime(call.startMs);
  }

  function ended(): void {
    // Past the video's end the clock plays on, over its last frame.
    sync();
  }

  // Keeps the first highlighted call in view, scrolling the trace only,
  // never the page.
  $effect(() => {
    const first = active[0];
    if (first === undefined || list === undefined) return;
    const row = rows[first];
    if (row === undefined) return;
    const top = row.offsetTop - list.offsetTop;
    if (top < list.scrollTop || top + row.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTo({ top: top - list.clientHeight / 3, behavior: "smooth" });
    }
  });

  function clock(ms: number): string {
    const seconds = ms / 1000;
    const minutes = Math.floor(seconds / 60);
    return `${minutes}:${(seconds - minutes * 60).toFixed(1).padStart(4, "0")}`;
  }

  function outcome(call: Call): string | null {
    const o = call.outcome;
    if (typeof o === "object") return `outcome ${o.unknown}`;
    switch (o) {
      case "done":
        return null;
      case "failed":
        return "failed";
      case "timed_out":
        return "timed out";
      case "refused":
        return "refused";
      default: {
        const never: never = o;
        return never;
      }
    }
  }

  function person(line: TraceLine): boolean {
    return line.kind === "call" && line.by === "person";
  }
</script>

<div class="replay">
  <div class="stage">
    {#if playing !== undefined}
      {#key current}
        <div class="screen" bind:clientWidth={boxWidth} bind:clientHeight={boxHeight}>
          <!-- The trace beside the video is its caption. -->
          <!-- svelte-ignore a11y_media_has_caption -->
          <video
            bind:this={player}
            src={videoUrl(recording.id, playing)}
            controls={!lined}
            muted
            playsinline
            preload="auto"
            onloadedmetadata={loaded}
            onended={ended}
          ></video>
          {#if unfilmed}<div class="unfilmed" aria-hidden="true"></div>{/if}
          {#if texting && drawn !== null && playing.points !== null && now !== null}
            {@const read = readAt(moments, now)}
            <Text
              shown={read === null ? { kind: "nothing" } : { kind: "read", read: read.read }}
              whose={read === null ? "" : `agent · ${((now - read.sinceMs) / 1000).toFixed(1)} s before`}
              stale={read?.actedSince ?? false}
              {drawn}
              display={playing.points}
            />
          {/if}
          {#if inspecting && seen !== null && drawn !== null && playing.points !== null}
            <Elements
              {seen}
              {refs}
              {drawn}
              display={playing.points}
              {wrappers}
              hovered={pointed}
              selected={picked}
              stale={found?.actedSince ?? false}
              picking={paused}
              onhover={(index) => (pointed = index)}
              onselect={(index) => (picked = index)}
            />
          {/if}
          {#if film !== null && drawn !== null && now !== null && playing.points != null}
            {@const frame = gave >= 0 ? film.over(playing.points, now) : null}
            {#if frame !== null}<Overlay {frame} {drawn} display={playing.points} />{/if}
          {/if}
        </div>
      {/key}
      {#if lined}
        <div class="transport">
          <button class="play" aria-label={paused ? "Play" : "Pause"} onclick={toggle} onkeydown={nudge}>
            {#if paused}<Play size={14} />{:else}<Pause size={14} />{/if}
          </button>
          <span class="clock">{clock(now ?? 0)}<span class="faint">&nbsp;/ {clock(length)}</span></span>
          {#if machines > 1}
            <div class="machines" role="group" aria-label="Whose screen">
              {#each names as name (name)}
                <button
                  class={["quiet", { on: name === shownMachine }]}
                  aria-pressed={name === shownMachine}
                  onclick={() => {
                    machine = name;
                    sync();
                  }}>{name}</button
                >
              {/each}
            </div>
          {/if}
        </div>
        <Timeline {moments} from={0} to={length} at={now} now={now ?? 0} label={clock} onseek={seekTime} />
        <div class="tools">
          <button
            class={["ghost", { on: inspecting }]}
            aria-pressed={inspecting}
            onclick={() => (inspecting = !inspecting)}
          >
            <ListTree size={15} />Elements the agent saw
          </button>
          <button class={["ghost", { on: texting }]} aria-pressed={texting} onclick={() => (texting = !texting)}>
            <ScanText size={15} />Text the agent read
          </button>
          {#if inspecting && paused === false}<span class="muted">Pause to point at elements on the video.</span>{/if}
        </div>
        {#if inspecting}
          <div class="elements">
            <Tree
              {seen}
              {refs}
              {attention}
              {empty}
              age={found === null || now === null ? "" : ago(now - found.sinceMs)}
              stale={found?.actedSince ?? false}
              {wrappers}
              hovered={pointed}
              selected={picked}
              onhover={(index) => (pointed = index)}
              onselect={(index) => (picked = index)}
              onwrappers={(next) => (wrappers = next)}
              onclose={() => (inspecting = false)}
            />
          </div>
        {/if}
      {/if}
      {#if !lined && recording.videos.length > 1}
        <div class="videos" role="group" aria-label="Videos">
          {#each recording.videos as video, index (video.file)}
            <button class={["quiet", { on: index === current }]} onclick={() => show(index, 0)}>
              {video.file.replace(/\.mp4$/, "")}
              {#if video.startMs !== null}<span class="muted">at {clock(video.startMs)}</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
    {:else}
      <p class="muted">No video: the session's machines weren't running while it was recorded.</p>
    {/if}
    {#if !lined && recording.videos.length > 0}
      <p class="note">
        Recorded before doppel kept when each video starts, so the trace can't follow the video.
      </p>
    {/if}
  </div>

  <ol class="trace" bind:this={list} aria-label="Trace">
    {#each recording.trace as line, index (index)}
      <li
        bind:this={rows[index]}
        class={["line", { active: active.includes(index), person: person(line) }]}
        aria-current={active.includes(index) ? "true" : undefined}
      >
        {#if line.kind === "call"}
          <button class="call" onclick={() => choose(line)} disabled={!lined}>
            <span class="time">{clock(line.startMs)}</span>
            <span class="text">
              {line.call}
              {#if machines > 1 && line.machine !== null}<span class="muted"> · {line.machine}</span>{/if}
            </span>
            {#if outcome(line) !== null}<span class="badge bad">{outcome(line)}</span>{/if}
            <span class="took muted">{((line.endMs - line.startMs) / 1000).toFixed(1)} s</span>
          </button>
          {#if line.error !== null}<p class="detail">{line.error}</p>{/if}
          {#if line.waitedMs !== null}<p class="detail muted">waited {(line.waitedMs / 1000).toFixed(1)} s for the machine first</p>{/if}
          {#if line.queuedMs !== null && line.queuedMs >= 100}<p class="detail muted">queued {(line.queuedMs / 1000).toFixed(1)} s behind other calls</p>{/if}
        {:else}
          <p class="detail muted">A line this page can't read: <code>{line.raw}</code></p>
        {/if}
      </li>
    {:else}
      <li class="muted">Nothing traced.</li>
    {/each}
  </ol>
</div>

<style>
  .transport {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 8px 0 6px;
  }
  .play {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: var(--slab);
    color: var(--on-slab);
    border: 0;
  }
  .clock {
    font: 500 12px/1 var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--strong);
  }
  .faint {
    color: var(--faint);
  }
  .machines {
    display: flex;
    gap: 4px;
    margin-left: auto;
  }
  .machines .on {
    box-shadow: inset 0 -2px 0 var(--live);
  }
  /* Before the machine's first video: its screen wasn't filmed yet. */
  .unfilmed {
    position: absolute;
    inset: 0;
    background: repeating-linear-gradient(135deg, var(--screen) 0 10px, var(--panel) 10px 11px);
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin-top: 8px;
    font-size: 12px;
  }
  .elements {
    display: flex;
    height: 320px;
    margin-top: 8px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .elements > :global(.tree) {
    flex: 1;
    border-left: 0;
  }
  .replay {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(280px, 1fr);
    gap: 16px;
    align-items: start;
  }
  @media (max-width: 860px) {
    .replay {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .screen {
    position: relative;
    aspect-ratio: 16 / 10;
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--screen);
  }
  video {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .videos {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
  .videos .on {
    border-color: var(--accent);
  }
  .note {
    margin: 8px 0 0;
    padding: 8px 12px;
    border-radius: 0;
    background: var(--notice);
    font-size: 13px;
  }
  .trace {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: min(70vh, 720px);
    overflow-y: auto;
    border: 1px solid var(--line);
    border-radius: 0;
    position: relative;
  }
  .line {
    border-bottom: 1px solid var(--line);
    font-size: 13px;
  }
  .line:last-child {
    border-bottom: 0;
  }
  .line.person {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .line.active {
    box-shadow: inset 3px 0 0 var(--accent);
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .call {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto auto;
    gap: 8px;
    align-items: baseline;
    width: 100%;
    padding: 8px 10px;
    border: 0;
    border-radius: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-transform: none;
    letter-spacing: 0;
    text-align: left;
    cursor: pointer;
  }
  .call:disabled {
    cursor: default;
  }
  .time,
  .took {
    font-variant-numeric: tabular-nums;
  }
  .time {
    color: var(--muted);
  }
  .text {
    overflow-wrap: anywhere;
  }
  .badge {
    font-size: 12px;
    padding: 0 6px;
    border-radius: 0;
  }
  .bad {
    background: var(--notice);
  }
  .detail {
    margin: 0;
    padding: 0 10px 8px 10px;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted);
  }
</style>

<script lang="ts">
  import type { Input } from "./gen/doppel/common/v1/common_pb";
  import { Eye, EyeOff, Film, Hand, LoaderCircle, MousePointer2, Pause, Play, Plus, TriangleAlert } from "@lucide/svelte";
  import Text from "./Text.svelte";
  import Elements from "./Elements.svelte";
  import Tree from "./Tree.svelte";
  import Menu, { type MenuItem } from "./Menu.svelte";
  import Screen from "./Screen.svelte";
  import { actionLook } from "./actions";
  import { type Action, look, machineActions, shown as isShown, underWay } from "./lib/lifecycle";
  import type { Rect, Size } from "./lib/geometry";
  import { type Machine, type MachineName, provenance, template } from "./lib/machines";
  import type { Report } from "./lib/report";
  import { type Look, type Moment, reading, sceneAt, showing, type TextLook } from "./lib/timeline";
  import type { TextRead } from "./lib/reading";
  import { type Seen, refsOf } from "./lib/tree";
  import Timeline from "./Timeline.svelte";
  import type { Session } from "./lib/sessions";
  import { hue } from "./lib/hues";
  import { describe } from "./lib/overlay";
  import type { Token } from "./lib/token";

  interface Props {
    machine: Machine;
    token: Token;
    /** The agent sessions whose requests named this machine. */
    drivers: Session[];
    /** Whether this page has control of it. */
    controlling: boolean;
    /** The action this page has under way on it, if any. */
    busy: Action | null;
    /** Its report, read every few seconds; `null` until the first is read. */
    report: Report | null;
    /** Calls agents started on it in the last minute. */
    pace: number;
    /** The machines recorded as forked from it. */
    forks: MachineName[];
    /** Holds an agent's calls, or lets them carry on. */
    onhold: (session: Session, to: "pause" | "resume") => void;
    onaction: (action: Action) => void;
    ontake: () => void;
    onhandback: () => void;
    /** Puts another machine on stage beside this one. */
    onshow: (name: MachineName) => void;
    /** Opens this machine's recordings. */
    onrecordings: () => void;
    send: (input: Input) => void;
    /** A person's own read of its screen text, which costs the agents driving it nothing. */
    observeText: () => Promise<{ kind: "read"; read: TextRead } | { kind: "failed"; why: string }>;
    /** A person's look at its elements, which costs the agents driving it nothing. */
    observe: () => Promise<{ kind: "seen"; seen: Seen } | { kind: "failed"; why: string }>;
    /** What agents did on it since the page opened. */
    moments: readonly Moment[];
    /** The page's clock. */
    now: number;
    /** How far back its timeline shows. */
    windowMs: number;
  }

  let {
    machine,
    token,
    drivers,
    controlling,
    busy,
    report,
    pace,
    forks,
    onhold,
    onaction,
    ontake,
    onhandback,
    onshow,
    onrecordings,
    send,
    observe,
    observeText,
    moments,
    now,
    windowMs,
  }: Props = $props();
  const scene = $derived(moments.length === 0 ? null : sceneAt(moments, now));
  /** Whether the agent's overlay is drawn over the screen. */
  let overlay = $state(true);
  /** Where the screen's picture is drawn, for the rim and its tab to hug. */
  let picture: Rect | null = $state(null);
  /**
   * What the screen is looked at through: the screen alone; the elements the
   * agent last saw (their boxes over it, their tree in the rail); or the text
   * read off it.
   */
  type Lens = "screen" | "elements" | "text";
  const LENSES: readonly Lens[] = ["screen", "elements", "text"];
  let lens = $state<Lens>("screen");
  const inspecting = $derived(lens === "elements");
  const texting = $derived(lens === "text");
  let wrappers: "show" | "hide" = $state("hide");
  /** The element pointed at, on the screen or in the tree, and the one picked. */
  let pointed: number | null = $state(null);
  let picked: number | null = $state(null);
  /** The person's own latest look at the elements. */
  let mine: Look | null = $state(null);
  /** The newer of the agent's last snapshot (live, it comes with its call) and the person's look. */
  const showingNow = $derived(showing(moments, mine, now));
  const seen = $derived.by(() => {
    switch (showingNow.kind) {
      case "agent":
        return showingNow.found.source.kind === "live" ? showingNow.found.source.seen : null;
      case "person":
        return showingNow.look.outcome.kind === "seen" ? showingNow.look.outcome.seen : null;
      case "nothing":
        return null;
      default: {
        const never: never = showingNow;
        return never;
      }
    }
  });
  /** The agent's refs; a person's look has none. */
  const refs = $derived(seen !== null && showingNow.kind === "agent" ? refsOf(seen, showingNow.found.moment.refs) : null);
  const stale = $derived(
    showingNow.kind === "agent" ? showingNow.found.actedSince : showingNow.kind === "person" ? showingNow.actedSince : false,
  );
  const age = $derived(
    showingNow.kind === "agent"
      ? `agent · ${ago(now - showingNow.found.sinceMs)}`
      : showingNow.kind === "person"
        ? `you · ${ago(now - showingNow.look.atMs)}`
        : "",
  );
  const empty = $derived(
    showingNow.kind === "person" && showingNow.look.outcome.kind !== "seen" ? showingNow.look.outcome : { kind: "none" as const },
  );

  /** The person's own latest read of the screen's text. */
  let myRead: TextLook | null = $state(null);
  const readingNow = $derived(reading(moments, myRead, now));
  const textShown = $derived.by(() => {
    switch (readingNow.kind) {
      case "agent":
        return {
          shown: { kind: "read" as const, read: readingNow.found.read },
          whose: `agent · ${ago(now - readingNow.found.sinceMs)}`,
          stale: readingNow.found.actedSince,
        };
      case "person":
        return {
          shown: readingNow.look.outcome,
          whose: `you · ${ago(now - readingNow.look.atMs)}`,
          stale: readingNow.actedSince,
        };
      case "nothing":
        return { shown: { kind: "nothing" as const }, whose: "", stale: false };
      default: {
        const never: never = readingNow;
        return never;
      }
    }
  });

  /** Reads the screen's text for the person: the host's view of the display, so the agents lose nothing. */
  async function readNow(): Promise<void> {
    const atMs = now;
    myRead = { atMs, outcome: { kind: "loading" } };
    const outcome = await observeText();
    if (myRead?.atMs === atMs) myRead = { atMs, outcome };
  }

  /** Takes the person's own look: the agents driving the machine lose nothing by it. */
  async function lookNow(): Promise<void> {
    const atMs = now;
    mine = { atMs, outcome: { kind: "loading" } };
    const outcome = await observe();
    // Only the latest look's answer counts.
    if (mine?.atMs === atMs) mine = { atMs, outcome };
  }
  // A new snapshot has other elements: what was picked in the last one goes.
  $effect(() => {
    void seen;
    pointed = null;
    picked = null;
  });

  function ago(ms: number): string {
    const seconds = Math.max(0, Math.round(ms / 1000));
    return seconds < 60 ? `${seconds} s ago` : `${Math.round(seconds / 60)} min ago`;
  }

  const allowed = $derived(machineActions(machine));
  /**
   * The labelled actions in the head. A machine with no screen shows its main
   * action in the middle of the panel instead, so it's offered once.
   */
  const shown = $derived(
    allowed.filter((action) => isShown(action) && (machine.state.kind === "live" || (action !== "start" && action !== "from-template"))),
  );
  const from = $derived(provenance(machine));
  /** Whether the rail shows: only with something in it, agents or the elements tree. */
  const railed = $derived(machine.state.kind === "live" && (drivers.length > 0 || lens === "elements"));
  const golden = $derived(template(machine));
  const menu: MenuItem[] = $derived([
    { label: "Recordings", icon: Film, onselect: onrecordings },
    ...allowed
      .filter((action) => !isShown(action))
      .map((action) => {
        const { label, icon, danger } = actionLook(action);
        return { label, icon, danger, onselect: () => onaction(action) };
      }),
  ]);
  const shownState = $derived(look(machine.state));
  const running = $derived(machine.state.kind === "live" && machine.state.run === "running");
  const live = $derived(machine.state.kind === "live");

  function who(session: Session): string {
    return session.client === "" ? "An agent" : session.client;
  }

  /**
   * The call an agent is on: from the live feed the overlay draws, which
   * sees every call as it starts; else from the session list, polled every
   * few seconds, which misses quick ones.
   */
  function onCall(session: Session): { what: string; since: number } | null {
    const current = scene?.agents.find((agent) => agent.session === Number(session.id))?.current ?? null;
    if (current !== null) return { what: describe(current.call), since: current.since };
    const polled = session.running[0];
    return polled === undefined ? null : { what: polled, since: -Infinity };
  }

  /** What an agent is doing now, in a few words. */
  function doing(session: Session): string {
    switch (session.activity.kind) {
      case "active":
        return onCall(session)?.what ?? "Idle";
      case "paused":
        return "Held";
      case "unknown":
        return `State ${session.activity.raw}`;
      default: {
        const never: never = session.activity;
        return never;
      }
    }
  }

  /** Who the screen's tab names: the person driving, an agent acting, or no one. */
  type Tab = { kind: "you" } | { kind: "agent"; session: Session; what: string } | { kind: "none" };
  const tab: Tab = $derived.by(() => {
    if (controlling) return { kind: "you" };
    // The agent whose call started last.
    let acting: Tab = { kind: "none" };
    let latest = -Infinity;
    for (const driver of drivers) {
      if (driver.activity.kind !== "active") continue;
      const call = onCall(driver);
      if (call === null || (acting.kind === "agent" && call.since <= latest)) continue;
      acting = { kind: "agent", session: driver, what: call.what };
      latest = call.since;
    }
    return acting;
  });

  /** When the person took control, for the tab's timer; `null` while they don't have it. */
  let tookAt: number | null = $state(null);
  let clock = $state(0);
  $effect(() => {
    if (!controlling) {
      tookAt = null;
      return;
    }
    tookAt = Date.now();
    clock = tookAt;
    const ticking = setInterval(() => (clock = Date.now()), 1000);
    return () => clearInterval(ticking);
  });
  const driven = $derived.by(() => {
    const seconds = tookAt === null ? 0 : Math.max(0, Math.floor((clock - tookAt) / 1000));
    return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  });
</script>

{#snippet controlLabel(label: string)}
  <span class="labels"><span>{label}</span><span class="spare" aria-hidden="true">{label === "Hand back" ? "Take control" : "Hand back"}</span></span>
{/snippet}

{#snippet layers(drawn: Rect, display: Size)}
  {#if texting}
    <Text
      shown={textShown.shown}
      whose={textShown.whose}
      stale={textShown.stale}
      {drawn}
      {display}
      looking={myRead?.outcome.kind === "loading" ? "busy" : "ready"}
      onread={() => void readNow()}
    />
  {/if}
  {#if inspecting && seen !== null}
    <Elements
      {seen}
      {refs}
      {drawn}
      {display}
      {wrappers}
      hovered={pointed}
      selected={picked}
      {stale}
      picking={!controlling}
      onhover={(index) => (pointed = index)}
      onselect={(index) => (picked = index)}
    />
  {/if}
{/snippet}

<article class={["panel", { controlling }]}>
  <!-- The same order in every panel: who it is, how it's looked at, what to do with it. -->
  <header class="head">
    <h2 title={machine.name}>{machine.name}</h2>
    {#if golden !== null}<span class="tag" title={`New machines start from its checkpoint ${golden}`}>Template</span>{/if}
    {#if busy !== null}
      <span class="state busy"><LoaderCircle size={11} class="spin" />{underWay(busy)}</span>
    {:else}
      <span class={["state", shownState.tone]}><span class="dot"></span>{shownState.label}</span>
    {/if}
    {#if live}
      <div class="lenses" role="radiogroup" aria-label="Lens">
        {#each LENSES as option (option)}
          <button role="radio" aria-checked={lens === option} class={{ on: lens === option }} onclick={() => (lens = option)}>{option}</button>
        {/each}
      </div>
      <button
        class={["toggle", { off: !overlay }]}
        title={overlay ? "Hide what the agents do" : "Show what the agents do"}
        aria-label={overlay ? "Hide the agents' overlay" : "Show the agents' overlay"}
        aria-pressed={overlay}
        onclick={() => (overlay = !overlay)}
      >
        {#if overlay}<Eye size={14} />{:else}<EyeOff size={14} />{/if}
      </button>
    {/if}
    <span class="fill"></span>
    {#each shown as action (action)}
      {@const { label, icon: Icon } = actionLook(action)}
      <button class="tool" title={label} disabled={busy !== null} onclick={() => onaction(action)}>
        <Icon size={14} /><span class="tool-label">{label}</span>
      </button>
    {/each}
    {#if menu.length > 0}
      <span class="tool-menu"><Menu label={`More for ${machine.name}`} items={menu} /></span>
    {/if}
    {#if running}
      {#if controlling}
        <button class="control back" onclick={onhandback}><Hand size={14} />{@render controlLabel("Hand back")}</button>
      {:else}
        <button class="control" onclick={ontake}><MousePointer2 size={14} />{@render controlLabel("Take control")}</button>
      {/if}
    {/if}
  </header>

  <!-- The rail is there only when there's something in it: agents, or the elements tree. -->
  <div class={["body", { railed }]}>
    <div
      class={["frame", tab.kind, { live, placed: picture !== null }]}
      style:--frame={tab.kind === "agent" ? hue(Number(tab.session.id)).fill : undefined}
      style:--pic-left="{picture?.left ?? 0}px"
      style:--pic-top="{picture?.top ?? 0}px"
      style:--pic-width="{picture?.width ?? 0}px"
      style:--pic-height="{picture?.height ?? 0}px"
    >
      {#if live}
        <!-- The rim names who has the screen: you, an agent, or no one. Always there, so the screen never jumps. -->
        <div class="rim">
          {#if tab.kind === "you"}
            <span class="tab"><MousePointer2 size={11} />You <span class="clock">{driven}</span></span>
          {:else if tab.kind === "agent"}
            <span class="tab" title={tab.what}>{who(tab.session)} <span class="what">{tab.what}</span></span>
          {/if}
        </div>
        <Screen name={machine.name} {token} {controlling} {send} scene={overlay ? scene : null} {now} layer={(inspecting && seen !== null) || texting ? layers : undefined} bind:picture />
        {#if tab.kind !== "none"}<div class="ring" aria-hidden="true"></div>{/if}
        {#if machine.state.kind === "live" && machine.state.run === "paused"}
          <div class="veil">
            <button class="primary" disabled={busy !== null} onclick={() => onaction("resume")}><Play size={14} />Resume</button>
          </div>
        {/if}
      {:else if machine.state.kind === "unreadable"}
        <div class="empty">
          <TriangleAlert size={28} />
          <p>Its status can't be read: {machine.state.reason}</p>
        </div>
      {:else}
        <!-- A machine with no screen: what it is, and its one main action. -->
        <div class="card">
          {#if golden !== null}
            <h3>Template · {golden}</h3>
            <dl>
              <dt>Checkpoints</dt>
              <dd>{report === null ? "…" : report.checkpoints}</dd>
              <dt>Made from it</dt>
              <dd>
                {#if forks.length === 0}None yet{:else}
                  {#each forks as fork, index (fork)}{#if index > 0}, {/if}<button class="link" onclick={() => onshow(fork)}>{fork}</button>{/each}
                {/if}
              </dd>
            </dl>
            <button class="primary" disabled={busy !== null} onclick={() => onaction("from-template")}><Plus size={14} />New machine from it</button>
          {:else}
            <h3>Stopped</h3>
            <dl>
              <dt>Checkpoints</dt>
              <dd>{report === null ? "…" : report.checkpoints}</dd>
              <dt>Origin</dt>
              <dd>
                {#if from?.kind === "said"}{from.text}{:else if from?.kind === "unreadable"}Unreadable: {from.reason}{:else}Not recorded{/if}
              </dd>
            </dl>
            <button class="primary" disabled={busy !== null} onclick={() => onaction("start")}><Play size={14} />Start</button>
          {/if}
        </div>
      {/if}
    </div>

    {#if railed}
      <aside class="rail">
        {#if drivers.length > 0}
          <h3>Agents · {drivers.length}</h3>
          <ul class="agents">
            {#each drivers as driver (driver.id)}
              {@const colours = hue(Number(driver.id))}
              <li class={["agent", { held: driver.activity.kind === "paused" || controlling }]}>
                <div class="who">
                  <span class="chip" style:background={colours.fill} aria-hidden="true"></span>
                  <span class="client">{who(driver)}</span>
                  <span class="calls" title="Calls answered">{driver.answered}</span>
                  {#if driver.activity.kind === "paused"}
                    <button class="hold" title="Let it carry on" aria-label={`Resume ${who(driver)}`} onclick={() => onhold(driver, "resume")}><Play size={12} /></button>
                  {:else}
                    <button class="hold" title="Hold its calls" aria-label={`Pause ${who(driver)}`} onclick={() => onhold(driver, "pause")}><Pause size={12} /></button>
                  {/if}
                </div>
                <span class="now">{#if controlling}<Pause size={11} /> Waits for you{:else}{doing(driver)}{/if}</span>
              </li>
            {/each}
          </ul>
        {/if}
        {#if inspecting}
          <Tree
            {seen}
            {refs}
            {empty}
            looking={mine?.outcome.kind === "loading" ? "busy" : "ready"}
            onlook={() => void lookNow()}
            {age}
            {stale}
            {wrappers}
            hovered={pointed}
            selected={picked}
            onhover={(index) => (pointed = index)}
            onselect={(index) => (picked = index)}
            onwrappers={(next) => (wrappers = next)}
            onclose={() => (lens = "screen")}
          />
        {/if}
      </aside>
    {/if}
  </div>

  {#if live}
    <!-- Always there, so the screen never jumps when the first call comes. -->
    <div class="tape">
      <Timeline
        {moments}
        from={now - windowMs}
        to={now}
        {now}
        label={(ms) => (Math.round((ms - now) / 1000) === 0 ? "now" : `${Math.round((ms - now) / 1000)} s`)}
      />
      {#if moments.length === 0}<span class="quiet">No calls yet</span>{/if}
    </div>
  {/if}

  <!-- Facts about this machine: glanced at, never acted on. -->
  <footer class="facts">
    {#if live && report !== null}
      {@const guest = report.guest}
      {#if guest.kind === "answering"}
        <span class="fact">{guest.login === "locked" ? "Screen locked" : "Guest ready"}</span>
        {#if guest.front !== null}<span class="fact strong">{guest.front}</span>{/if}
        {#if guest.display !== null}<span class="fact">{guest.display.width}×{guest.display.height}</span>{/if}
      {:else if guest.kind === "unreachable"}
        <span class="fact warn" title={guest.why}>Guest agent unreachable</span>
      {:else}
        <span class="fact">Guest agent not up yet</span>
      {/if}
    {/if}
    {#if report !== null}
      <span class="fact">{report.checkpoints} {report.checkpoints === 1 ? "checkpoint" : "checkpoints"}</span>
    {/if}
    {#if from?.kind === "said" && golden === null}
      <span class="fact">{from.text}</span>
    {:else if from?.kind === "unreadable"}
      <span class="fact warn" title={from.reason}>Lineage unreadable</span>
    {/if}
    <span class="fill"></span>
    {#if live}
      {#if drivers.length === 0}<span class="fact end">No agents</span>{/if}
      <span class="fact end" title="Calls agents started on it in the last minute">{pace} / min</span>
    {/if}
  </footer>
</article>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    background: var(--surface);
    container-type: inline-size;
  }
  /* The screen's rim: none while no one has it, an agent's colour while it acts, the signal while you drive. */
  .frame.you {
    --frame: var(--signal);
  }
  /* Under the rim's tab, with as much room below, so the picture is centred. */
  .frame.live :global(.screen),
  .frame.live > .veil {
    top: 22px;
    bottom: 22px;
  }
  /* The rim hugs the picture, not the bars beside it; the whole screen until the picture's place is known. */
  .ring {
    position: absolute;
    top: 22px;
    left: 0;
    right: 0;
    bottom: 22px;
    z-index: 2;
    box-shadow: inset 0 0 0 2px var(--frame);
    pointer-events: none;
  }
  .placed .ring {
    top: calc(22px + var(--pic-top));
    left: var(--pic-left);
    right: auto;
    bottom: auto;
    width: var(--pic-width);
    height: var(--pic-height);
  }
  .frame.live {
    background: var(--ground);
  }
  /* The tab sits on the picture's top left corner. */
  .rim {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 22px;
    z-index: 3;
    display: flex;
    align-items: flex-end;
    pointer-events: none;
  }
  .placed .rim {
    left: var(--pic-left);
    top: var(--pic-top);
    right: auto;
    width: var(--pic-width);
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 70%;
    height: 22px;
    padding: 0 8px;
    background: var(--frame);
    color: #05080a;
    font: 600 11.5px/1 var(--sans);
    white-space: nowrap;
    overflow: hidden;
  }
  .frame.you .tab {
    color: var(--on-signal);
  }
  .tab .what {
    font-weight: 500;
    opacity: 0.8;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tab .clock {
    font: 500 11px/1 var(--mono);
  }

  .head {
    display: flex;
    align-items: stretch;
    height: 36px;
    flex: none;
    border-bottom: 1px solid var(--line);
    font: 500 11px/1 var(--mono);
    background: var(--raised);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  h2 {
    min-width: 0;
    max-width: 30cqw;
    margin: 0;
    padding: 0 12px;
    font: 600 13px/36px var(--mono);
    text-transform: none;
    letter-spacing: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    display: block;
  }
  /* A role, not a warning: neutral. */
  .tag {
    align-self: center;
    margin-right: 10px;
    padding: 3px 5px;
    border: 1px solid var(--line-strong);
    color: var(--muted);
    font: 500 9.5px/1 var(--mono);
  }
  .state {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 12px;
    border-left: 1px solid var(--line);
    border-right: 1px solid var(--line);
    color: var(--muted);
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
    flex: none;
    background: currentColor;
  }
  .running {
    color: var(--success);
  }
  .paused {
    color: var(--warning);
  }
  .busy {
    color: var(--accent);
  }
  .stopped {
    color: var(--muted);
  }
  .error {
    color: var(--danger);
  }
  .lenses {
    display: flex;
    border-right: 1px solid var(--line);
  }
  .head button {
    height: 100%;
    border: 0;
    border-right: 1px solid var(--line);
    background: transparent;
    color: var(--muted);
    padding: 0 12px;
  }
  .head button:hover {
    background: var(--hover);
    color: var(--text);
  }
  .lenses button.on {
    background: var(--slab);
    color: var(--on-slab);
  }
  .toggle.off {
    color: var(--faint);
  }
  .fill {
    flex: 1;
  }
  .head .tool {
    border-right: 0;
    border-left: 1px solid var(--line);
  }
  .head .tool {
    gap: 6px;
  }
  .tool-menu {
    display: flex;
    align-items: center;
    padding: 0 3px;
    border-left: 1px solid var(--line);
  }
  .tool-menu :global(button) {
    border: 0;
  }
  .head .control {
    border-right: 0;
    padding: 0 16px;
    justify-content: center;
    background: var(--signal);
    color: var(--on-signal);
  }
  .head .control:hover {
    background: color-mix(in srgb, var(--signal) 85%, var(--strong));
    color: var(--on-signal);
  }
  .head .control.back {
    background: transparent;
    color: var(--signal);
    box-shadow: inset 0 0 0 1px var(--signal);
  }
  .head .control.back:hover {
    background: color-mix(in srgb, var(--signal) 12%, transparent);
    color: var(--signal);
  }

  /* Both labels take the same room, so the head never shifts as control changes hands. */
  .labels {
    display: inline-grid;
  }
  .labels > span {
    grid-area: 1 / 1;
    text-align: center;
  }
  .labels > .spare {
    visibility: hidden;
  }

  /* The screen, and beside it (below it, in a narrow panel) the rail. */
  .body {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    flex: 1;
    min-height: 0;
  }
  .body.railed {
    grid-template-columns: minmax(0, 1fr) 264px;
  }
  .frame {
    position: relative;
    min-height: 0;
    min-width: 0;
    background: var(--screen);
    overflow: hidden;
  }
  .rail {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-left: 1px solid var(--line);
    overflow: hidden;
    background: var(--raised);
  }
  .rail h3 {
    flex: none;
    margin: 0;
    padding: 9px 14px;
    border-bottom: 1px solid var(--line);
    font: 500 10.5px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .none {
    margin: 0;
    padding: 12px 14px;
    font-size: 12px;
    color: var(--muted);
  }
  .agents {
    flex: none;
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 50%;
    overflow: auto;
  }
  .agent {
    display: grid;
    gap: 4px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
  }
  .agent.held .now {
    color: var(--muted);
  }
  .agent.held .chip {
    opacity: 0.45;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font: 500 11px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .chip {
    flex: none;
    width: 10px;
    height: 10px;
  }
  .client {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .calls {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .hold {
    width: 22px;
    height: 22px;
    padding: 0;
    justify-content: center;
    border: 1px solid var(--line);
    background: transparent;
    color: var(--muted);
  }
  .now {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rail > :global(.tree) {
    flex: 1;
    min-height: 0;
  }

  .tape {
    position: relative;
    flex: none;
    padding: 6px 12px 2px;
    border-top: 1px solid var(--line);
  }
  .quiet {
    position: absolute;
    left: 12px;
    top: 8px;
    font: 400 10.5px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--faint);
  }

  .facts {
    display: flex;
    flex: none;
    height: 24px;
    min-width: 0;
    overflow: hidden;
    border-top: 1px solid var(--line);
    background: var(--raised);
    font: 400 10.5px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--muted);
    white-space: nowrap;
  }
  .fact {
    display: flex;
    align-items: center;
    padding: 0 12px;
    border-right: 1px solid var(--line);
  }
  .fact.end {
    border-right: 0;
    border-left: 1px solid var(--line);
  }
  .fact.strong {
    color: var(--text);
    text-transform: none;
    letter-spacing: 0;
  }
  .fact.warn {
    color: var(--warning);
  }

  @container (max-width: 860px) {
    .body.railed {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(0, 1fr) auto;
    }
    .rail {
      border-left: 0;
      border-top: 1px solid var(--line);
      max-height: 45cqh;
    }
    .lenses button {
      padding: 0 8px;
    }
    .tool-label {
      display: none;
    }
  }
  @container (max-width: 520px) {
    .fact:not(.end):not(.strong) {
      display: none;
    }
    /* Two rows: the machine and what to do with it, then how to look at it. */
    .head {
      flex-wrap: wrap;
      height: auto;
    }
    .head > :global(*),
    .head button {
      height: 36px;
    }
    .head::before {
      content: "";
      order: 2;
      flex-basis: 100%;
    }
    h2 {
      max-width: 50cqw;
    }
    .lenses,
    .toggle,
    .tool {
      order: 3;
      border-top: 1px solid var(--line);
    }
    .lenses {
      flex: 1;
    }
    .lenses button {
      flex: 1;
    }
  }

  .veil {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--screen) 55%, transparent);
    backdrop-filter: grayscale(0.8);
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 10px;
    padding: 16px;
    text-align: center;
    color: var(--faint);
  }
  .card {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: start;
    gap: 14px;
    padding: 24px;
    max-width: 420px;
    margin: 0 auto;
    color: var(--on-slab);
  }
  .card h3 {
    margin: 0;
    font: 500 11px/1 var(--mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: color-mix(in srgb, var(--on-slab) 60%, transparent);
  }
  .card p {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
  }
  .card dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 16px;
    margin: 0;
    font: 400 12px/1.3 var(--mono);
  }
  .card dt {
    color: color-mix(in srgb, var(--on-slab) 60%, transparent);
    text-transform: uppercase;
    font-size: 10.5px;
    letter-spacing: 0.04em;
  }
  .card dd {
    margin: 0;
  }
  .link {
    display: inline;
    height: auto;
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-transform: none;
    letter-spacing: 0;
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .empty p {
    margin: 0;
    color: var(--muted);
    font-size: 13px;
  }
</style>

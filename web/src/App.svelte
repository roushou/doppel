<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { create, toBinary } from "@bufbuild/protobuf";
  import { Columns2, LoaderCircle, LogOut, Monitor, Moon, Plus, Sun, X } from "@lucide/svelte";
  import { type ThemeChoice, nextTheme, readTheme, shownTheme, themeLabel } from "./lib/theme";
  import type { Input } from "./gen/doppel/common/v1/common_pb";
  import {
    ActivitySchema,
    GetMachineReportSchema,
    HandBackSchema,
    ListMachinesSchema,
    ListSessionsSchema,
    ObserveElementsSchema,
    ObserveTextSchema,
    PauseSessionSchema,
    ResumeSessionSchema,
    SendHostInputSchema,
    TakeControlSchema,
    WatchActivitySchema,
  } from "./gen/doppel/daemon/v1/daemon_pb";
  import Ask, { type Ask as Asking } from "./Ask.svelte";
  import Launcher from "./Launcher.svelte";
  import Panel from "./Panel.svelte";
  import Recordings from "./Recordings.svelte";
  import { type Closed, Connection, ConnectError, type Operation, refusal } from "./lib/connection";
  import { type Action, checkpointLabel, look } from "./lib/lifecycle";
  import { type Machine, type MachineName, forksOf, parseMachines, template } from "./lib/machines";
  import { type Label, type Order, operation, verdict } from "./lib/operate";
  import { MARK_MS, parseActivity } from "./lib/overlay";
  import { type Film, Films } from "./lib/film";
  import filmModule from "./gen/film.wasm?url";
  import { calloutsAt } from "./lib/timeline";
  import { callsPerMinute, parseReport } from "./lib/report";
  import { type Seen, readSnapshot } from "./lib/tree";
  import { type TextRead, readLines } from "./lib/reading";
  import { type Session, parseSessions } from "./lib/sessions";
  import {
    type Panels,
    begin,
    disconnected,
    finish,
    handedBack,
    held,
    keepReportsOf,
    machineOf,
    panelOf,
    recorded,
    reported,
    took,
  } from "./lib/panels";
  import {
    type Bay,
    type Stage,
    bays,
    closeLauncher,
    escape,
    full,
    keepBays,
    readBays,
    settle,
    showSlot as showing,
    shows,
    slotOf,
    stageOf,
    startFrom,
    toggleLauncher,
    toggleSplit,
  } from "./lib/stage";
  import { type Token, parseToken, tokenCookie, tokenFromFragment } from "./lib/token";

  type Page =
    | { kind: "no-token" }
    | { kind: "connecting" }
    | { kind: "connected" }
    | { kind: "closed"; closed: Closed };

  type Toast = { id: number; tone: "done" | "refused" | "unknown"; text: string };

  const KEPT = "doppel-token";
  const BAYS_KEPT = "doppel-bays";
  const THEME_KEPT = "doppel-theme";

  /** The theme chosen, and whether the system is light: what the page shows follows both. */
  let theme: ThemeChoice = $state(readKeptTheme());
  const systemLight = window.matchMedia("(prefers-color-scheme: light)");
  let light = $state(systemLight.matches);
  systemLight.addEventListener("change", (event) => (light = event.matches));
  $effect.pre(() => {
    const shown = shownTheme(theme, light);
    document.documentElement.dataset.theme = shown;
  });

  function readKeptTheme(): ThemeChoice {
    try {
      return readTheme(localStorage.getItem(THEME_KEPT));
    } catch {
      return "system";
    }
  }

  function switchTheme(): void {
    theme = nextTheme(theme);
    try {
      localStorage.setItem(THEME_KEPT, theme);
    } catch {
      // Not kept: this load only.
    }
  }

  let page: Page = $state({ kind: "connecting" });
  let view: "machines" | "recordings" = $state("machines");
  /** The machine whose recordings the Recordings view lists first; `null` for all. */
  let recordingsOf: MachineName | null = $state(null);
  /** How the bays and their machines are laid out. */
  let layout: Stage = $state(stageOf([]));
  let slotsBar: HTMLElement | undefined = $state();
  let machines: Machine[] = $state([]);
  let sessions: Session[] = $state([]);
  /** What the page keeps about each machine: its action under way, control, report and timeline. */
  let panels: Panels = $state(new Map());
  /** The machines this page has control of. */
  const controlled = $derived(held(panels));
  let asking: Asking | null = $state(null);
  let toasts: Toast[] = $state([]);
  /** Reads the staged machines' reports again every few seconds. */
  let reporting: ReturnType<typeof setInterval> | undefined;
  let token: Token | null = $state(null);
  let control: Connection | null = null;
  /** The connection agents' activity streams on. */
  let watching: Connection | null = null;
  /** The page's clock for the timelines, ticking while one has something moving. */
  let now = $state(0);
  let ticking = false;
  /** The film's module, loading; none if it can't be: the screens then show no overlay. */
  const films: Promise<Films | null> = Films.instantiate(fetch(filmModule)).catch((error: unknown) => {
    console.warn("the overlay's film didn't load:", error);
    return null;
  });
  /** Each machine's live film, made as its first activity comes. */
  let live: ReadonlyMap<MachineName, Film> = $state.raw(new Map());
  /** How far back a live timeline shows. */
  const WINDOW_MS = 60_000;
  let refresh: ReturnType<typeof setInterval> | undefined;
  let toastId = 0;

  const shown: Bay[] = $derived(bays(machines, layout.kept));
  /** The machines on the stage: the picked slot's alone, or both split. */
  const stage = $derived(shows(layout, shown));
  const staged = $derived(shown.flatMap((bay) => (bay.kind === "machine" ? [bay.machine.name] : [])));
  /** What an empty bay can start: every machine not running. */
  const idle = $derived(machines.filter((machine) => machine.state.kind !== "live"));

  /** Reads the report of every machine on stage, for its panel's footer. */
  async function readReports(): Promise<void> {
    panels = keepReportsOf(panels, staged);
    await Promise.all(
      staged.map(async (name) => {
        const outcome = await control?.request({ case: "getMachineReport", value: create(GetMachineReportSchema, { name }) });
        panels = reported(panels, name, outcome?.case === "machineReport" ? parseReport(outcome.value) : null, staged);
      }),
    );
  }

  /** Lays the stage out as `next`, keeping what each bay holds for the next load. */
  function lay(next: Stage): void {
    const moved = next.kept.some((name, index) => name !== layout.kept[index]);
    layout = next;
    if (!moved) return;
    try {
      localStorage.setItem(BAYS_KEPT, keepBays(next.kept));
    } catch {
      // Not kept: this load only.
    }
  }

  function readKeptBays(): (MachineName | null)[] {
    try {
      return readBays(localStorage.getItem(BAYS_KEPT) ?? "");
    } catch {
      return [null, null];
    }
  }

  // A machine that took a bay stays in it across loads.
  $effect(() => {
    const next = settle(layout, shown);
    if (next !== layout) lay(next);
  });

  // The bays' machines' reports, read as they change.
  $effect(() => {
    void staged.join();
    void Promise.resolve().then(readReports);
  });

  /** Starts `name` in slot `index`: it lands there once it's running, and shows. */
  function startHere(index: number, name: MachineName): void {
    lay(startFrom(layout, index, name));
    pick(name, "start");
  }

  /** Shows slot `index`'s machine alone. */
  function showSlot(index: number): void {
    lay(showing(layout, index));
    if (view !== "machines") void show("machines");
  }

  function outsideSlots(event: PointerEvent): void {
    if (slotsBar !== undefined && !slotsBar.contains(event.target as Node)) lay(closeLauncher(layout));
  }

  function readToken(): Token | null {
    const { token: fromLink, rest } = tokenFromFragment(location.hash);
    if (fromLink !== null) {
      history.replaceState(null, "", `${location.pathname}${location.search}${rest}`);
      try {
        sessionStorage.setItem(KEPT, fromLink);
      } catch {
        // Kept for this load only.
      }
      return fromLink;
    }
    try {
      return parseToken(sessionStorage.getItem(KEPT) ?? "");
    } catch {
      return null;
    }
  }

  function toast(tone: Toast["tone"], text: string): void {
    const id = ++toastId;
    toasts = [...toasts, { id, tone, text }];
    // A refusal or an unknown outcome stays until it's dismissed.
    if (tone === "done") setTimeout(() => dismiss(id), 4500);
  }

  function dismiss(id: number): void {
    toasts = toasts.filter((shown) => shown.id !== id);
  }

  async function list(): Promise<void> {
    const [machineList, sessionList] = await Promise.all([
      control?.request({ case: "listMachines", value: create(ListMachinesSchema) }),
      control?.request({ case: "listSessions", value: create(ListSessionsSchema) }),
    ]);
    if (machineList?.case === "machineList") machines = parseMachines(machineList.value);
    if (sessionList?.case === "sessionList") sessions = parseSessions(sessionList.value);
  }

  /** Pauses or resumes a session, then reads the lists again to show it. */
  async function hold(session: Session, to: "pause" | "resume"): Promise<void> {
    const outcome =
      to === "pause"
        ? await control?.request({ case: "pauseSession", value: create(PauseSessionSchema, { session: session.id }) })
        : await control?.request({ case: "resumeSession", value: create(ResumeSessionSchema, { session: session.id }) });
    if (outcome !== undefined && outcome.case !== "done") toast("refused", `${who(session)}: ${refusal(outcome)}`);
    await list();
  }

  /** The sessions whose requests named a machine. */
  function drivers(name: MachineName): Session[] {
    return sessions.filter((session) => session.machines.includes(name));
  }

  function who(session: Session): string {
    return session.client === "" ? "An agent" : session.client;
  }

  async function connect(): Promise<void> {
    if (token === null) {
      page = { kind: "no-token" };
      return;
    }
    page = { kind: "connecting" };
    try {
      control = await Connection.open(Connection.socketUrl(location), token);
    } catch (error) {
      const closed = error instanceof ConnectError ? error.closed : { code: 0, reason: String(error) };
      page = { kind: "closed", closed };
      return;
    }
    control.onClose((closed) => {
      clearInterval(refresh);
      clearInterval(reporting);
      panels = disconnected(panels);
      page = { kind: "closed", closed };
    });
    page = { kind: "connected" };
    void watch();
    await list();
    // The daemon has no machine events yet: the list is read again.
    refresh = setInterval(() => void list(), 2000);
    void readReports();
    reporting = setInterval(() => void readReports(), 5000);
  }

  /** Watches what agents do, for the overlay; tries again while the page is connected. */
  async function watch(): Promise<void> {
    if (token === null || page.kind !== "connected") return;
    try {
      watching = await Connection.open(Connection.socketUrl(location), token);
    } catch {
      setTimeout(() => void watch(), 3000);
      return;
    }
    watching.onClose(() => {
      watching = null;
      setTimeout(() => void watch(), 3000);
    });
    const made = await films;
    await watching.stream({ case: "watchActivity", value: create(WatchActivitySchema) }, (event) => {
      if (event.kind.case !== "activity") return;
      const read = parseActivity(event.kind.value);
      if (read === null) return;
      now = performance.now();
      panels = recorded(panels, read, now);
      if (made !== null) told(made, machineOf(read), toBinary(ActivitySchema, event.kind.value));
      tick();
    });
  }

  /** Tells `name`'s live film of an activity event at `now`, making the film first if it's the machine's first. */
  function told(made: Films, name: MachineName, activity: Uint8Array): void {
    let film = live.get(name);
    if (film === undefined) {
      film = made.live();
      live = new Map(live).set(name, film);
    }
    try {
      film.told(activity, now);
    } catch (error) {
      // One this build can't read, as the page's own timeline skips it.
      console.warn(`the overlay skipped an event on ${name}:`, error);
    }
  }

  /**
   * Moves the clock: every frame while a mark fades or a call is under way,
   * a few times a second while a timeline still shows a call, and not at
   * all once every timeline is empty.
   */
  function tick(): void {
    if (ticking) return;
    ticking = true;
    const frame = () => {
      now = performance.now();
      let latest = -Infinity;
      let open = false;
      for (const { timeline: moments } of panels.values()) {
        for (const moment of moments) {
          if (moment.endMs === null) open = true;
          latest = Math.max(latest, moment.endMs ?? now, moment.startMs);
        }
        // An agent's mark showing fades out smoothly.
        if (calloutsAt(moments, now).length > 0) open = true;
      }
      if (open || now - latest < MARK_MS) requestAnimationFrame(frame);
      else if (now - latest < WINDOW_MS) setTimeout(frame, 250);
      else ticking = false;
    };
    requestAnimationFrame(frame);
  }

  async function take(name: MachineName): Promise<void> {
    const outcome = await control?.request(
      { case: "takeControl", value: create(TakeControlSchema, { name }) },
      `take control of ${name}`,
    );
    if (outcome?.case === "done") panels = took(panels, name);
    else if (outcome !== undefined) toast("refused", `${name}: ${refusal(outcome)}`);
  }

  async function handBack(name: MachineName): Promise<void> {
    const outcome = await control?.request(
      { case: "handBack", value: create(HandBackSchema, { name }) },
      `hand ${name} back`,
    );
    if (outcome?.case !== "done" && outcome !== undefined) toast("refused", `${name}: ${refusal(outcome)}`);
    // Dropped whatever the answer: the daemon refuses a hand back only when
    // this connection doesn't hold the machine, and a lost connection hands
    // back everything it held.
    panels = handedBack(panels, name);
  }

  async function send(name: MachineName, input: Input): Promise<void> {
    const outcome = await control?.request({
      case: "sendHostInput",
      value: create(SendHostInputSchema, { name, input }),
    });
    if (outcome !== undefined && outcome.case !== "done" && outcome.case !== undefined) {
      toast("refused", `${name}: input refused: ${refusal(outcome)}`);
    }
  }

  /** A request on the page's connection, for a part of the page that has none. */
  /** A person's look at `name`'s elements. */
  async function observe(name: MachineName): Promise<{ kind: "seen"; seen: Seen } | { kind: "failed"; why: string }> {
    const outcome = await control?.request({ case: "observeElements", value: create(ObserveElementsSchema, { name }) });
    if (outcome?.case === "axSnapshot") return { kind: "seen", seen: readSnapshot(outcome.value) };
    return { kind: "failed", why: outcome === undefined ? "not connected" : refusal(outcome) };
  }

  /** A person's own read of `name`'s screen text. */
  async function observeText(name: MachineName): Promise<{ kind: "read"; read: TextRead } | { kind: "failed"; why: string }> {
    const outcome = await control?.request({ case: "observeText", value: create(ObserveTextSchema, { name }) });
    if (outcome?.case === "screenText") return { kind: "read", read: { lines: readLines(outcome.value), query: null, found: [] } };
    return { kind: "failed", why: outcome === undefined ? "not connected" : refusal(outcome) };
  }

  function request(operation: Operation) {
    return control?.request(operation) ?? Promise.resolve(undefined);
  }

  async function checkpoints(name: MachineName): Promise<Label[] | string> {
    const outcome = await control?.request({ case: "getMachineReport", value: create(GetMachineReportSchema, { name }) });
    if (outcome === undefined) return "not connected";
    if (outcome.case !== "machineReport") return refusal(outcome);
    return outcome.value.checkpoints.flatMap((raw) => {
      const label = checkpointLabel(raw);
      return label === null ? [] : [label];
    });
  }

  /** Starts an action the person picked: one that needs more asks for it first. */
  function pick(name: MachineName, action: Action): void {
    const golden = machines.find((machine) => machine.name === name);
    const goldenTemplate = golden === undefined ? null : template(golden);
    switch (action) {
      case "start-golden":
        // No longer golden by the time it's picked: started as asked.
        if (goldenTemplate === null) void run(name, { action: "start" });
        else asking = { kind: "start-golden", machine: name, template: goldenTemplate };
        return;
      case "from-template":
        asking = { kind: "choose", action: "new-copy", machine: name, template: goldenTemplate };
        return;
      case "start":
      case "stop":
      case "pause":
      case "resume":
        void run(name, { action });
        return;
      case "force-stop":
      case "delete":
        asking = { kind: action, machine: name };
        return;
      case "checkpoint":
        asking = { kind: "checkpoint", machine: name };
        return;
      case "roll-back":
      case "restore":
      case "fork":
      case "new-copy":
        asking = { kind: "choose", action, machine: name, template: null };
        return;
      default: {
        const never: never = action;
        return never;
      }
    }
  }

  /** Whether an action ends what a person in control is doing, so control is handed back first. */
  function endsControl(action: Action): boolean {
    switch (action) {
      case "pause":
      case "resume":
      case "checkpoint":
      case "fork":
      case "new-copy":
      case "from-template":
        return false;
      case "start":
      case "start-golden":
      case "stop":
      case "force-stop":
      case "roll-back":
      case "restore":
      case "delete":
        return true;
      default: {
        const never: never = action;
        return never;
      }
    }
  }

  async function run(name: MachineName, order: Order): Promise<void> {
    asking = null;
    const started = begin(panels, name, order.action);
    if (started === null) return;
    panels = started;
    try {
      if (panelOf(panels, name).control === "held" && endsControl(order.action)) await handBack(name);
      const outcome = await control?.request(operation(name, order));
      if (outcome === undefined) return;
      const read = verdict(outcome);
      switch (read.kind) {
        case "done":
          toast("done", read.note === null ? `${name}: ${done(order)}` : `${name}: ${read.note}`);
          break;
        case "refused":
          toast("refused", `${name}: ${read.reason}`);
          break;
        case "unknown":
          toast("unknown", `${name}: it may or may not have happened. ${read.reason}`);
          break;
        default: {
          const never: never = read;
          return never;
        }
      }
    } finally {
      panels = finish(panels, name);
      await list();
    }
  }

  function done(order: Order): string {
    switch (order.action) {
      case "start":
        return "started";
      case "stop":
      case "force-stop":
        return "stopped";
      case "pause":
        return "paused";
      case "resume":
        return "resumed";
      case "checkpoint":
        return `saved checkpoint ${order.label}`;
      case "roll-back":
        return `rolled back to ${order.label}`;
      case "restore":
        return `restored ${order.label}`;
      case "fork":
        return `forked into ${order.into}`;
      case "new-copy":
        return `new machine started from ${order.label}`;
      case "delete":
        return "deleted";
      default: {
        const never: never = order;
        return never;
      }
    }
  }

  /** Shows a view; leaving the machines hands back those this page controls, so no agent waits on a screen no one watches. */
  async function show(next: "machines" | "recordings"): Promise<void> {
    if (next === "recordings") await Promise.all(controlled.map((name) => handBack(name)));
    view = next;
  }

  function forget(): void {
    try {
      sessionStorage.removeItem(KEPT);
    } catch {
      // Nothing was kept.
    }
    document.cookie = tokenCookie(null);
    token = null;
    control?.close();
    watching?.close();
    page = { kind: "no-token" };
  }

  onMount(() => {
    layout = stageOf(readKeptBays());
    token = readToken();
    if (token !== null) document.cookie = tokenCookie(token);
    void connect();
  });
  onDestroy(() => {
    clearInterval(refresh);
    clearInterval(reporting);
    control?.close();
    watching?.close();
  });
</script>

<svelte:window
  onpointerdown={outsideSlots}
  onkeydown={(event) => {
    if (event.key !== "Escape") return;
    // Escape leaves the split, unless it's meant for a screen someone drives or a dialog.
    const meant = asking === null && controlled.length === 0 && view === "machines" ? "stage" : "elsewhere";
    lay(escape(layout, meant));
  }}
/>

{#snippet brand()}
  <span class="logo" aria-hidden="true"><span></span><span></span></span>
{/snippet}

{#if page.kind !== "connected"}
  <div class="gate">
    <span class="brand gate-brand">{@render brand()}</span>
    {#if page.kind === "no-token"}
      <h1>This page needs a token</h1>
      <p>On the Mac, run <code>doppel daemon token</code> and open the link it prints. The token is kept for this tab only.</p>
    {:else if page.kind === "connecting"}
      <p class="connecting"><LoaderCircle size={16} class="spin" /> Connecting…</p>
    {:else}
      <h1>Disconnected</h1>
      <p>{page.closed.reason === "" ? `The connection closed (${page.closed.code}).` : `${page.closed.reason}.`}</p>
      <div class="row">
        <button class="primary" onclick={() => void connect()}>Reconnect</button>
        <button class="ghost" onclick={forget}>Use another token</button>
      </div>
    {/if}
  </div>
{:else}
  <div class="app">
    <header class="top">
      <span class="brand" title="doppel">{@render brand()}<span class="wordmark">doppel</span></span>
      <nav class="segments" aria-label="View">
        <button aria-current={view === "machines" ? "page" : undefined} class={{ on: view === "machines" }} onclick={() => void show("machines")}>Machines</button>
        <button
          aria-current={view === "recordings" ? "page" : undefined}
          class={{ on: view === "recordings" }}
          onclick={() => {
            recordingsOf = null;
            void show("recordings");
          }}>Recordings</button
        >
      </nav>
      {#if machines.length > 0}
        <!-- Two slots: Apple runs at most two macOS machines at a time. -->
        <nav class="slots" aria-label="Running machines" bind:this={slotsBar}>
          {#each shown as bay, index (index)}
            {#if bay.kind === "machine"}
              {@const on = view === "machines" && stage.includes(bay.machine)}
              <button class={["slot", { on }]} aria-current={on ? "true" : undefined} onclick={() => showSlot(index)}>
                <span class={["dot", panelOf(panels, bay.machine.name).control === "held" ? "driven" : look(bay.machine.state).tone]}></span>{bay.machine.name}
              </button>
            {:else}
              <span class="free">
                <button
                  class={["slot", "add", { open: layout.launching === index }]}
                  title="Start a machine"
                  aria-label="Start a machine"
                  aria-expanded={layout.launching === index}
                  onclick={() => lay(toggleLauncher(layout, index))}><Plus size={14} /></button
                >
                {#if layout.launching === index}
                  <div class="launcher" role="dialog" aria-label="Start a machine">
                    <Launcher
                      machines={idle}
                      busy={(name) => panelOf(panels, name).action}
                      onstart={(name) => startHere(index, name)}
                      onaction={(name, action) => {
                        lay(closeLauncher(layout));
                        pick(name, action);
                      }}
                      onnew={() => {
                        lay(closeLauncher(layout));
                        asking = { kind: "choose", action: "new-copy", machine: null, template: null };
                      }}
                      onrecordings={(name) => {
                        lay(closeLauncher(layout));
                        recordingsOf = name;
                        void show("recordings");
                      }}
                    />
                  </div>
                {/if}
              </span>
            {/if}
          {/each}
          {#if full(shown) && view === "machines"}
            <button
              class={["slot", "split", { on: layout.split === "on" }]}
              title={layout.split === "on" ? "One at a time (Esc)" : "Side by side"}
              aria-label="Side by side"
              aria-pressed={layout.split === "on"}
              onclick={() => lay(toggleSplit(layout))}><Columns2 size={14} /></button
            >
          {/if}
        </nav>
      {/if}
      <span class="fill"></span>
      <span class="live" title="Connected to the daemon"><span class="dot round running"></span>Live</span>
      <button class="bar-button icon-only" title={themeLabel(theme)} aria-label={themeLabel(theme)} onclick={switchTheme}>
        {#if theme === "light"}<Sun size={14} />{:else if theme === "dark"}<Moon size={14} />{:else}<Monitor size={14} />{/if}
      </button>
      <button class="bar-button icon-only" title="Sign out" aria-label="Sign out" onclick={forget}><LogOut size={14} /></button>
    </header>

    <main>
      {#if view === "recordings"}
        <div class="page">
          {#key recordingsOf}<Recordings {request} machine={recordingsOf} {films} />{/key}
        </div>
      {:else if machines.length === 0}
        <div class="nothing">
          <Monitor size={36} />
          <h2>No machines yet</h2>
          <p>Make one on the Mac with <code>doppel create</code>; it shows here as soon as it exists.</p>
        </div>
      {:else if token !== null}
        {#if stage.length === 0}
          <!-- Nothing running: what can start, where the screen will be. -->
          <div class="idle">
            <Launcher
              machines={idle}
              busy={(name) => panelOf(panels, name).action}
              onstart={(name) => startHere(0, name)}
              onaction={(name, action) => pick(name, action)}
              onnew={() => (asking = { kind: "choose", action: "new-copy", machine: null, template: null })}
              onrecordings={(name) => {
                recordingsOf = name;
                void show("recordings");
              }}
            />
          </div>
        {:else}
          <div class={["stage", stage.length === 1 ? "single" : "pair"]}>
            {#each stage as machine (machine.name)}
              <Panel
                {machine}
                {token}
                drivers={drivers(machine.name)}
                controlling={panelOf(panels, machine.name).control === "held"}
                busy={panelOf(panels, machine.name).action}
                moments={panelOf(panels, machine.name).timeline}
                film={live.get(machine.name) ?? null}
                {now}
                windowMs={WINDOW_MS}
                report={panelOf(panels, machine.name).report}
                pace={callsPerMinute(panelOf(panels, machine.name).timeline.map((moment) => moment.startMs), now)}
                forks={forksOf(machines, machine.name)}
                onaction={(action) => pick(machine.name, action)}
                ontake={() => void take(machine.name)}
                onhandback={() => void handBack(machine.name)}
                onhold={(session, to) => void hold(session, to)}
                onrecordings={() => {
                  recordingsOf = machine.name;
                  void show("recordings");
                }}
                onshow={(name) => {
                  const at = slotOf(shown, name);
                  if (at !== null) showSlot(at);
                }}
                send={(input) => void send(machine.name, input)}
                observe={() => observe(machine.name)}
                observeText={() => observeText(machine.name)}
              />
            {/each}
          </div>
        {/if}
      {/if}
    </main>
  </div>

  {#if asking !== null}
    <Ask
      ask={asking}
      {machines}
      {checkpoints}
      now={() => new Date()}
      onsubmit={(name, order) => void run(name, order)}
      onclose={() => (asking = null)}
    />
  {/if}

  <ol class="toasts" aria-live="polite">
    {#each toasts as shown (shown.id)}
      <li class={["toast", shown.tone]}>
        <span>{shown.text}</span>
        <button class="ghost icon" aria-label="Dismiss" onclick={() => dismiss(shown.id)}><X size={14} /></button>
      </li>
    {/each}
  </ol>
{/if}

<style>
  /* Two squares: a machine and its double. */
  .logo {
    position: relative;
    display: block;
    width: 20px;
    height: 20px;
  }
  .logo span {
    position: absolute;
    width: 11px;
    height: 11px;
  }
  .logo span:first-child {
    left: 2px;
    top: 2px;
    border: 1.5px solid var(--strong);
  }
  .logo span:last-child {
    left: 7px;
    top: 7px;
    background: var(--live);
  }

  .gate {
    max-width: 460px;
    margin: 18vh auto 0;
    padding: 0 16px;
    display: grid;
    gap: 14px;
    color: var(--muted);
  }
  .gate-brand {
    width: 40px;
    height: 40px;
  }
  .gate h1 {
    margin: 12px 0 0;
    color: var(--text);
    font-size: 22px;
    letter-spacing: -0.02em;
  }
  .gate p {
    margin: 0;
  }
  .connecting {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .row {
    display: flex;
    gap: 8px;
  }

  .app {
    display: grid;
    grid-template-rows: 44px minmax(0, 1fr);
    height: 100dvh;
  }

  .top {
    display: flex;
    align-items: stretch;
    min-width: 0;
    border-bottom: 1px solid var(--line);
    background: var(--ground);
  }
  .brand {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 16px 0 12px;
    border-right: 1px solid var(--line);
  }
  .wordmark {
    font: 700 12px/1 var(--mono);
    letter-spacing: 0.3em;
    text-transform: uppercase;
    color: var(--strong);
  }
  .fill {
    flex: 1;
  }
  .live {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 14px;
    font: 500 11px/1 var(--mono);
    letter-spacing: var(--track);
    text-transform: uppercase;
    color: var(--live);
  }

  .segments {
    display: flex;
    border-right: 1px solid var(--line);
  }
  .bar-button {
    height: 100%;
    gap: 8px;
    padding: 0 14px;
    border: 0;
    border-left: 1px solid var(--line);
    background: transparent;
    color: var(--text);
  }
  .bar-button:hover {
    background: var(--hover);
  }
  .bar-button.icon-only {
    width: 40px;
    padding: 0;
    justify-content: center;
    color: var(--muted);
  }
  .segments button {
    height: 100%;
    padding: 0 14px;
    border: 0;
    border-right: 1px solid var(--line);
    background: transparent;
    color: var(--muted);
  }
  .segments button:last-child {
    border-right: 0;
  }
  .segments button:hover {
    background: var(--hover);
    color: var(--text);
  }
  .segments button.on {
    background: transparent;
    color: var(--strong);
    box-shadow: inset 0 -2px 0 var(--live);
  }

  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    background: var(--faint);
  }
  .dot.round {
    border-radius: 50%;
  }
  .dot.running {
    background: var(--success);
  }
  .dot.driven {
    background: var(--signal);
  }
  .dot.paused {
    background: var(--warning);
  }
  .dot.busy {
    background: var(--accent);
    animation: pulse 1.2s ease-in-out infinite;
  }
  .dot.error {
    background: var(--danger);
  }
  :global(.busy-dot) {
    flex: none;
    color: var(--accent);
  }

  main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .page {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 16px;
  }
  .stage {
    flex: 1;
    min-height: 0;
    display: grid;
  }
  .stage.single {
    grid-template-columns: minmax(0, 1fr);
  }
  .stage.pair {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  .stage.pair > :global(.panel + .panel) {
    border-left: 1px solid var(--line);
  }
  /* The slots: a tab per running machine, a + for a free one. */
  .slots {
    display: flex;
    align-items: stretch;
    min-width: 0;
  }
  .free {
    position: relative;
    display: flex;
  }
  .slot {
    height: 100%;
    gap: 8px;
    padding: 0 14px;
    border: 0;
    border-right: 1px solid var(--line);
    background: transparent;
    color: var(--muted);
    font: 500 12px/1 var(--mono);
    text-transform: none;
    letter-spacing: 0;
    white-space: nowrap;
  }
  .slot:hover {
    background: var(--hover);
    color: var(--text);
  }
  .slot.on {
    color: var(--strong);
    background: var(--panel);
    box-shadow: inset 0 -2px 0 var(--live);
  }
  .slot.add,
  .slot.split {
    width: 44px;
    padding: 0;
    justify-content: center;
  }
  .slot.add.open {
    background: var(--panel);
    color: var(--strong);
  }
  .launcher {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 20;
    width: 380px;
    max-width: calc(100vw - 16px);
    max-height: 70vh;
    overflow: auto;
    padding: 14px;
    border: 1px solid var(--line-strong);
    background: var(--panel);
  }
  .idle {
    flex: 1;
    display: grid;
    place-content: center;
    grid-template-columns: min(420px, calc(100vw - 32px));
    padding: 24px 16px;
  }
  .nothing {
    flex: 1;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 6px;
    padding: 24px;
    text-align: center;
    color: var(--faint);
  }
  .nothing h2 {
    margin: 8px 0 0;
    font-size: 16px;
    color: var(--text);
  }
  .nothing p {
    margin: 0;
    color: var(--muted);
  }

  .toasts {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 50;
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 8px;
    width: min(380px, calc(100vw - 32px));
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 6px 10px 14px;
    border: 1px solid var(--line-strong);
    border-left-width: 3px;
    border-radius: var(--radius);
    background: var(--overlay);
    box-shadow: var(--shadow);
    font-size: 13px;
    animation: slide 0.2s var(--ease);
  }
  .toast span {
    flex: 1;
    padding-top: 5px;
    overflow-wrap: anywhere;
  }
  .toast.done {
    border-left-color: var(--success);
  }
  .toast.refused {
    border-left-color: var(--danger);
  }
  .toast.unknown {
    border-left-color: var(--warning);
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }

  /* A phone or a narrow window: the stage stacks, and the header keeps what fits. */
  @media (max-width: 860px) {
    .wordmark {
      display: none;
    }
    /* The slots take a row of their own under the header. */
    .app {
      grid-template-rows: auto minmax(0, 1fr);
    }
    .top {
      flex-wrap: wrap;
    }
    .top > :global(*) {
      height: 44px;
    }
    .slots {
      order: 9;
      flex-basis: 100%;
      border-top: 1px solid var(--line);
    }
    .slot:not(.add):not(.split) {
      flex: 1;
      justify-content: center;
    }
    .launcher {
      left: auto;
      right: 0;
    }
    .stage {
      align-content: start;
      overflow: auto;
    }
    .stage.pair {
      grid-template-columns: minmax(0, 1fr);
    }
    .stage :global(.frame) {
      flex: none;
      aspect-ratio: 16 / 10;
    }
  }
</style>

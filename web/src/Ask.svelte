<script lang="ts" module>
  import type { MachineName } from "./lib/machines";
  import type { Label } from "./lib/operate";

  /** What the page asks before an action runs. */
  export type Ask =
    | { kind: "checkpoint"; machine: MachineName }
    /**
     * Pick a checkpoint; with no machine, pick the machine first (a new machine).
     * `template`, a golden image's, is chosen once read when it's among them.
     */
    | {
        kind: "choose";
        action: "roll-back" | "restore" | "fork" | "new-copy";
        machine: MachineName | null;
        template: Label | null;
      }
    | { kind: "force-stop"; machine: MachineName }
    /** That starting a golden image, which lets it drift from its template, is meant. */
    | { kind: "start-golden"; machine: MachineName; template: Label }
    | { kind: "delete"; machine: MachineName };
</script>

<script lang="ts">
  import { Bookmark, LoaderCircle } from "@lucide/svelte";
  import Dialog from "./Dialog.svelte";
  import { checkpointLabel, machineName, preselected } from "./lib/lifecycle";
  import type { Machine } from "./lib/machines";
  import type { Order } from "./lib/operate";

  interface Props {
    ask: Ask;
    machines: Machine[];
    /** A machine's checkpoints, or why they can't be read. */
    checkpoints: (name: MachineName) => Promise<Label[] | string>;
    /** Today's date, for a label's suggestion. */
    now: () => Date;
    onsubmit: (machine: MachineName, order: Order) => void;
    onclose: () => void;
  }

  let { ask, machines, checkpoints, now, onsubmit, onclose }: Props = $props();

  type Loaded = { kind: "loading" } | { kind: "loaded"; labels: Label[] } | { kind: "failed"; reason: string };

  function suggestion(): string {
    const at = now();
    const two = (n: number) => String(n).padStart(2, "0");
    return `cp-${at.getFullYear()}${two(at.getMonth() + 1)}${two(at.getDate())}-${two(at.getHours())}${two(at.getMinutes())}`;
  }

  // svelte-ignore state_referenced_locally
  let source: MachineName | null = $state(ask.kind === "choose" && ask.machine === null ? (machines[0]?.name ?? null) : null);
  const machine = $derived(ask.kind === "choose" && ask.machine === null ? source : ask.machine);
  let loaded: Loaded = $state({ kind: "loading" });
  let chosen: Label | null = $state(null);
  // svelte-ignore state_referenced_locally
  let typed = $state(ask.kind === "checkpoint" ? suggestion() : "");
  /** The fork's name: suggested from the machine and checkpoint until the person edits it. */
  let into = $derived(
    machine === null || chosen === null ? "" : `${machine}-${chosen}`.replaceAll("_", "-").slice(0, 32).replace(/-+$/, ""),
  );

  // Reading the checkpoints is a request: it can't be derived.
  $effect(() => {
    if (ask.kind !== "choose" || machine === null) return;
    const name = machine;
    loaded = { kind: "loading" };
    chosen = null;
    void checkpoints(name).then((read) => {
      if (name !== machine) return;
      loaded = typeof read === "string" ? { kind: "failed", reason: read } : { kind: "loaded", labels: read };
      if (typeof read !== "string") chosen = preselected(read, ask.kind === "choose" ? ask.template : null);
    });
  });


  const title = $derived.by(() => {
    switch (ask.kind) {
      case "checkpoint":
        return `Save a checkpoint of ${ask.machine}`;
      case "force-stop":
        return `Force stop ${ask.machine}?`;
      case "start-golden":
        return `Start ${ask.machine}?`;
      case "delete":
        return `Delete ${ask.machine}?`;
      case "choose":
        switch (ask.action) {
          case "roll-back":
            return `Roll ${ask.machine} back`;
          case "restore":
            return `Restore ${ask.machine}`;
          case "fork":
            return `Fork ${ask.machine}`;
          case "new-copy":
            if (ask.machine === null) return "New machine";
            return ask.template === null ? `New copy of ${ask.machine}` : `New machine from ${ask.machine}`;
          default: {
            const never: never = ask.action;
            return never;
          }
        }
      default: {
        const never: never = ask;
        return never;
      }
    }
  });

  /** The order the form makes now, or null while it isn't complete. */
  const order: { machine: MachineName; order: Order } | null = $derived.by(() => {
    switch (ask.kind) {
      case "checkpoint": {
        const label = checkpointLabel(typed);
        return label === null ? null : { machine: ask.machine, order: { action: "checkpoint", label } };
      }
      case "force-stop":
        return { machine: ask.machine, order: { action: "force-stop" } };
      case "start-golden":
        return { machine: ask.machine, order: { action: "start" } };
      case "delete":
        return typed.trim() === ask.machine ? { machine: ask.machine, order: { action: "delete" } } : null;
      case "choose": {
        if (machine === null || chosen === null) return null;
        const label = chosen;
        switch (ask.action) {
          case "roll-back":
          case "restore":
          case "new-copy":
            return { machine, order: { action: ask.action, label } };
          case "fork": {
            const name = machineName(into);
            return name === null ? null : { machine, order: { action: "fork", label, into: name } };
          }
          default: {
            const never: never = ask.action;
            return never;
          }
        }
      }
      default: {
        const never: never = ask;
        return never;
      }
    }
  });

  const danger = $derived(
    ask.kind === "delete" ||
      ask.kind === "force-stop" ||
      (ask.kind === "choose" && (ask.action === "roll-back" || ask.action === "restore")),
  );

  const verb = $derived.by(() => {
    switch (ask.kind) {
      case "checkpoint":
        return "Save";
      case "force-stop":
        return "Force stop";
      case "start-golden":
        return "Start anyway";
      case "delete":
        return "Delete for good";
      case "choose":
        switch (ask.action) {
          case "roll-back":
            return "Roll back";
          case "restore":
            return "Restore";
          case "fork":
            return "Fork";
          case "new-copy":
            return "Create and start";
          default: {
            const never: never = ask.action;
            return never;
          }
        }
      default: {
        const never: never = ask;
        return never;
      }
    }
  });

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    if (order !== null) onsubmit(order.machine, order.order);
  }
</script>

<Dialog {title} {onclose}>
  <form id="ask" onsubmit={submit}>
    {#if ask.kind === "checkpoint"}
      <p>It saves the disk and memory now, then carries on. You can roll back to it, restore it, or start copies from it.</p>
      <label>
        Label
        <!-- svelte-ignore a11y_autofocus -->
        <input bind:value={typed} autofocus spellcheck="false" autocomplete="off" />
      </label>
      <small class={{ wrong: typed !== "" && checkpointLabel(typed) === null }}>
        Lowercase letters, digits, - and _.
      </small>
    {:else if ask.kind === "force-stop"}
      <p>It powers {ask.machine} off at once, as pulling the plug would. Anything unsaved in it is lost.</p>
    {:else if ask.kind === "start-golden"}
      <p>{ask.machine} is a golden image: new machines start from its checkpoint <code>{ask.template}</code>.</p>
      <p>
        Starting it lets it drift from that template: what changes in it isn't in <code>{ask.template}</code>, and new
        machines won't have it. To work in a copy, make a new machine from it instead.
      </p>
    {:else if ask.kind === "delete"}
      <p>
        This removes <strong>{ask.machine}</strong>, its disk and all its checkpoints, for good. Its recordings stay.
      </p>
      <label>
        Type <code>{ask.machine}</code> to confirm
        <!-- svelte-ignore a11y_autofocus -->
        <input bind:value={typed} autofocus spellcheck="false" autocomplete="off" />
      </label>
    {:else}
      {#if ask.machine === null}
        <p>A new machine starts from a checkpoint of one you have: its disk and login, under a new identity.</p>
        <label>
          From
          <select bind:value={source}>
            {#each machines as option (option.name)}
              <option value={option.name}>{option.name}</option>
            {/each}
          </select>
        </label>
      {:else if ask.action === "roll-back"}
        <p>{ask.machine} goes back to the checkpoint while it runs. Everything since is thrown away.</p>
      {:else if ask.action === "restore"}
        <p>{ask.machine}'s disk and memory go back to the checkpoint. Everything since is thrown away.</p>
      {:else if ask.action === "fork"}
        <p>A new stopped machine with the checkpoint's disk and memory. It shares {ask.machine}'s identity, so the two can't run at once.</p>
      {:else if ask.template !== null}
        <p>A new machine with the checkpoint's disk and login under a new identity, started. {ask.machine} stays as it is.</p>
      {:else}
        <p>A new machine with the checkpoint's disk and login under a new identity, started beside {ask.machine}.</p>
      {/if}
      <fieldset>
        <legend>Checkpoint</legend>
        {#if loaded.kind === "loading"}
          <p class="quiet"><LoaderCircle size={14} class="spin" /> Reading checkpoints…</p>
        {:else if loaded.kind === "failed"}
          <p class="wrong">{loaded.reason}</p>
        {:else if loaded.labels.length === 0}
          <p class="quiet">{machine} has no checkpoints yet.</p>
        {:else}
          <div class="labels">
            {#each loaded.labels as label (label)}
              <label class={["choice", { on: chosen === label }]}>
                <input type="radio" name="label" value={label} bind:group={chosen} />
                <Bookmark size={14} />
                {label}
              </label>
            {/each}
          </div>
        {/if}
      </fieldset>
      {#if ask.action === "fork"}
        <label>
          New machine's name
          <input bind:value={into} spellcheck="false" autocomplete="off" />
        </label>
      {/if}
    {/if}
  </form>
  {#snippet actions()}
    <button class="ghost" onclick={onclose}>Cancel</button>
    <button form="ask" type="submit" class={danger ? "danger" : "primary"} disabled={order === null}>{verb}</button>
  {/snippet}
</Dialog>

<style>
  form {
    display: grid;
    gap: 14px;
  }
  p {
    margin: 0;
  }
  strong,
  code {
    color: var(--text);
  }
  small {
    margin-top: -8px;
    font-size: 12px;
    color: var(--faint);
  }
  .wrong {
    color: var(--danger);
  }
  .quiet {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--faint);
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 6px;
  }
  legend {
    font-size: 13px;
    padding: 0 0 6px;
  }
  .labels {
    display: grid;
    gap: 4px;
    max-height: 220px;
    overflow: auto;
  }
  .choice {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--radius-sm);
    color: var(--text);
    font: 13px var(--mono);
    cursor: pointer;
  }
  .choice:hover {
    background: var(--hover);
  }
  .choice.on {
    border-color: var(--accent-fill);
    background: var(--accent-soft);
  }
  .choice input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }
</style>

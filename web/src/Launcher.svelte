<!-- What a free slot can start: the machines not running, each with its
     main action and menu, then New machine. -->
<script lang="ts">
  import { Film, LoaderCircle, Plus } from "@lucide/svelte";
  import { actionLook } from "./actions";
  import Menu, { type MenuItem } from "./Menu.svelte";
  import { type Action, machineActions, underWay } from "./lib/lifecycle";
  import { type Machine, type MachineName, template } from "./lib/machines";

  interface Props {
    /** The machines that aren't running: what a free slot can start. */
    machines: Machine[];
    /** The action under way on a machine, if any. */
    busy: (name: MachineName) => Action | null;
    /** Starts a machine in the free slot. */
    onstart: (name: MachineName) => void;
    onaction: (name: MachineName, action: Action) => void;
    onnew: () => void;
    onrecordings: (name: MachineName) => void;
  }

  let { machines, busy, onstart, onaction, onnew, onrecordings }: Props = $props();

  const stopped = $derived(machines.filter((machine) => template(machine) === null));
  const golden = $derived(machines.filter((machine) => template(machine) !== null));

  /** The row's main action, as a button; the rest go in its menu. */
  function main(machine: Machine): Action | null {
    const allowed = machineActions(machine);
    if (allowed.includes("from-template")) return "from-template";
    return allowed.includes("start") ? "start" : null;
  }

  function menu(machine: Machine): MenuItem[] {
    const first = main(machine);
    return [
      { label: "Recordings", icon: Film, onselect: () => onrecordings(machine.name) },
      ...machineActions(machine)
        .filter((action) => action !== first)
        .map((action) => {
          const { label, icon, danger } = actionLook(action);
          return { label, icon, danger, onselect: () => onaction(machine.name, action) };
        }),
    ];
  }

  function go(machine: Machine, action: Action): void {
    if (action === "start") onstart(machine.name);
    else onaction(machine.name, action);
  }
</script>

{#snippet row(machine: Machine)}
  {@const doing = busy(machine.name)}
  {@const first = main(machine)}
  {@const label = template(machine)}
  <li class="row">
    {#if doing !== null}
      <LoaderCircle size={10} class="spin" />
    {:else}
      <span class={["dot", machine.state.kind === "unreadable" ? "error" : "stopped"]}></span>
    {/if}
    <span class="name">{machine.name}</span>
    {#if label !== null}<span class="tag" title={`New machines start from its checkpoint ${label}`}>{label}</span>{/if}
    <span class="fill"></span>
    {#if doing !== null}
      <span class="doing">{underWay(doing)}</span>
    {:else if first !== null}
      {@const { label: words, icon: Icon } = actionLook(first)}
      <button class="go" title={words} aria-label={`${words}: ${machine.name}`} onclick={() => go(machine, first)}>
        <Icon size={13} />
      </button>
    {/if}
    <Menu label={`More for ${machine.name}`} items={menu(machine)} />
  </li>
{/snippet}

<div class="list">
  {#if stopped.length > 0}
    <h2 class="label-mono">Stopped <span class="count">{stopped.length}</span></h2>
    <ul>{#each stopped as machine (machine.name)}{@render row(machine)}{/each}</ul>
  {/if}
  {#if golden.length > 0}
    <h2 class="label-mono">Golden images <span class="count">{golden.length}</span></h2>
    <ul>{#each golden as machine (machine.name)}{@render row(machine)}{/each}</ul>
  {/if}
  <button class="new" onclick={onnew}><Plus size={14} />New machine</button>
</div>

<style>
  .list {
    width: 100%;
  }
  h2 {
    margin: 16px 0 6px;
    font-size: 11px;
    color: var(--muted);
  }
  h2:first-child {
    margin-top: 0;
  }
  .count {
    margin-left: 6px;
    color: var(--faint);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 40px;
    padding: 0 0 0 12px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  .dot {
    width: 7px;
    height: 7px;
    flex: none;
    background: var(--faint);
  }
  .dot.error {
    background: var(--danger);
  }
  .name {
    font: 500 13px/1 var(--mono);
    color: var(--strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    padding: 3px 5px;
    border: 1px solid var(--line-strong);
    font: 500 10px/1 var(--mono);
    color: var(--muted);
  }
  .fill {
    flex: 1;
  }
  .doing {
    font: 500 11px/1 var(--mono);
    color: var(--muted);
    padding-right: 8px;
  }
  .go {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    padding: 0;
    border: 0;
    border-left: 1px solid var(--line);
    background: transparent;
    color: var(--live);
  }
  .go:hover {
    background: var(--hover);
  }
  .row :global(.menu button) {
    height: 40px;
    border-left: 1px solid var(--line);
  }
  .new {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    width: 100%;
    height: 34px;
    margin-top: 14px;
    border: 1px solid var(--line-strong);
    background: transparent;
    color: var(--text);
  }
  .new:hover {
    background: var(--hover);
  }
</style>

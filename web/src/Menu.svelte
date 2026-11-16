<script lang="ts" module>
  import type { Component } from "svelte";

  export interface MenuItem {
    label: string;
    icon: Component<{ size?: number }>;
    /** Shown in the danger colour: it can't be undone. */
    danger?: boolean;
    onselect: () => void;
  }
</script>

<script lang="ts">
  import { Ellipsis } from "@lucide/svelte";

  interface Props {
    label: string;
    items: MenuItem[];
  }

  let { label, items }: Props = $props();
  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();

  function outside(event: PointerEvent): void {
    if (root !== undefined && !root.contains(event.target as Node)) open = false;
  }

  function key(event: KeyboardEvent): void {
    if (event.key === "Escape") open = false;
  }
</script>

<svelte:window onpointerdown={outside} onkeydown={key} />

<div class="menu" bind:this={root}>
  <button class="ghost icon" aria-label={label} aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>
    <Ellipsis size={16} />
  </button>
  {#if open}
    <div class="list" role="menu">
      {#each items as item (item.label)}
        {@const Icon = item.icon}
        <button
          role="menuitem"
          class={["item", { danger: item.danger === true }]}
          onclick={() => {
            open = false;
            item.onselect();
          }}
        >
          <Icon size={15} />
          {item.label}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .menu {
    position: relative;
  }
  .list {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 20;
    min-width: 200px;
    padding: 5px;
    display: grid;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    background: var(--overlay);
    box-shadow: var(--shadow);
    animation: drop 0.14s var(--ease);
  }
  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  .item {
    justify-content: flex-start;
    width: 100%;
    height: 32px;
    border: 0;
    background: transparent;
    color: var(--text);
    font-weight: 400;
  }
  .item:hover {
    background: var(--hover);
  }
  .item.danger {
    color: var(--danger);
  }
</style>

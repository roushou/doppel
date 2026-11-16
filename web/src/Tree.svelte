<script lang="ts">
  import { ChevronRight, LoaderCircle, ScanEye, X } from "@lucide/svelte";
  import { type Element, type Row, type Seen, ancestors, initiallyFolded, oneLine, reveal, rows } from "./lib/tree";

  interface Props {
    /** The snapshot shown; `null` while it loads or when there is none. */
    seen: Seen | null;
    /** The refs the agent's model was given for its elements (`s3e12`); `null` when it named none. */
    refs: ReadonlyMap<number, string> | null;
    /** Why there's no snapshot to show: none taken yet, it's loading, or it can't be read. */
    empty: { kind: "none" } | { kind: "loading" } | { kind: "failed"; why: string };
    /** How old it is, in words (`4 s ago`). */
    age: string;
    /** The machine was acted on since: the screen may have changed. */
    stale: boolean;
    wrappers: "show" | "hide";
    hovered: number | null;
    selected: number | null;
    onhover: (index: number | null) => void;
    onselect: (index: number | null) => void;
    onwrappers: (wrappers: "show" | "hide") => void;
    onclose: () => void;
    /** Whether the person can take their own look (live only), and whether one is under way. */
    looking?: "ready" | "busy";
    onlook?: () => void;
  }

  let { seen, refs, empty, age, stale, wrappers, hovered, selected, onhover, onselect, onwrappers, onclose, looking, onlook }: Props = $props();

  let list: HTMLOListElement | undefined = $state();
  const items: Record<number, HTMLLIElement> = $state({});
  /** How far each level is indented, and where its line runs, in pixels. */
  const INDENT = 16;
  const GUTTER = 6;
  /** The branches folded: menus at first, then as a person folds and opens them. */
  let folded: Set<number> = $state(new Set());
  // A new snapshot starts folded as a fresh one does.
  $effect.pre(() => {
    folded = seen === null ? new Set() : initiallyFolded(seen);
  });
  // An element picked on the screen inside a folded branch opens its branch.
  $effect(() => {
    if (seen === null || selected === null) return;
    if (ancestors(seen, selected).some((ancestor) => folded.has(ancestor))) folded = reveal(seen, folded, selected);
  });
  const shown = $derived(seen === null ? [] : rows(seen, wrappers, folded));
  /** The row of each element shown, by its index. */
  const at = $derived(new Map(shown.map((row, position) => [row.index, position])));
  /** The selected element's ancestors: their rows are marked, so its place in the tree reads at a glance. */
  const trail = $derived(new Set(seen === null || selected === null ? [] : ancestors(seen, selected)));
  const picked = $derived(selected === null ? null : (seen?.elements[selected] ?? null));

  /** The row standing for an element: its own, or, inside a folded branch, the nearest ancestor's shown. */
  function standIn(index: number): number | null {
    if (seen === null) return null;
    if (at.has(index)) return index;
    return ancestors(seen, index).find((ancestor) => at.has(ancestor)) ?? null;
  }

  function toggle(index: number): void {
    const next = new Set(folded);
    if (!next.delete(index)) next.add(index);
    folded = next;
  }

  /** Arrow keys move through the tree as a file browser's do; Enter folds or opens. */
  function keyed(event: KeyboardEvent): void {
    if (shown.length === 0) return;
    const position = selected === null ? -1 : (at.get(selected) ?? -1);
    const row: Row | undefined = shown[position];
    const go = (to: number) => {
      const next = shown[Math.min(shown.length - 1, Math.max(0, to))];
      if (next !== undefined) onselect(next.index);
    };
    switch (event.key) {
      case "ArrowDown":
        go(position + 1);
        break;
      case "ArrowUp":
        go(position === -1 ? 0 : position - 1);
        break;
      case "ArrowRight":
        if (row === undefined) go(0);
        else if (row.branch && !row.open) toggle(row.index);
        else if (row.branch) go(position + 1);
        break;
      case "ArrowLeft":
        if (row === undefined) go(0);
        else if (row.branch && row.open) toggle(row.index);
        else if (row.parent !== null) onselect(row.parent);
        break;
      case "Enter":
      case " ":
        if (row?.branch === true) toggle(row.index);
        break;
      case "Home":
        go(0);
        break;
      case "End":
        go(shown.length - 1);
        break;
      case "Escape":
        onselect(null);
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  /** A press on a row picks it; on its chevron, folds or opens it. */
  function pressed(event: MouseEvent, row: Row): void {
    if (event.target instanceof globalThis.Element && event.target.closest(".chevron") !== null) {
      toggle(row.index);
      return;
    }
    onselect(row.index === selected ? null : row.index);
  }

  // Keeps the element pointed at on the screen in view, scrolling the list only.
  $effect(() => {
    const pointed = hovered ?? selected;
    const index = pointed === null ? null : standIn(pointed);
    if (index === null || list === undefined) return;
    const row = items[index];
    if (row === undefined) return;
    const top = row.offsetTop - list.offsetTop;
    if (top < list.scrollTop || top + row.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTo({ top: top - list.clientHeight / 3 });
    }
  });

  function states(element: Element): string[] {
    const out: string[] = [];
    if (element.states.enabled === false) out.push("disabled");
    if (element.states.focused === true) out.push("focused");
    if (element.states.selected === true) out.push("selected");
    if (element.states.expanded === true) out.push("expanded");
    return out;
  }
</script>

<aside class="tree" aria-label="What the agent saw">
  <header>
    <div class="heading">
      <h3>{seen?.app?.name || "Elements"}</h3>
      {#if seen !== null}
        <span class={["age", { stale }]} title={stale ? "The machine was acted on since this snapshot: the screen may have changed." : "No action since this snapshot."}>
          {age}{#if stale} · acted since{/if}
        </span>
      {/if}
    </div>
    <div class="actions">
      {#if looking !== undefined && onlook !== undefined}
        <button
          class="ghost look"
          disabled={looking === "busy"}
          title="Read the elements on screen now, without disturbing the agents"
          onclick={onlook}
        >
          {#if looking === "busy"}<LoaderCircle size={13} class="spin" />Looking…{:else}<ScanEye size={13} />Look now{/if}
        </button>
      {/if}
      <button class="ghost icon" aria-label="Close the elements" title="Close" onclick={onclose}><X size={14} /></button>
    </div>
  </header>
  {#if seen === null}
    <p class="empty">
      {#if empty.kind === "loading"}
        <LoaderCircle size={14} class="spin" /> Loading the snapshot…
      {:else if empty.kind === "none"}
        No snapshot yet. The tree shows what the agent sees each time it takes one{#if onlook !== undefined}, or what you see with Look now{/if}.
      {:else}
        The snapshot can't be shown: {empty.why}
      {/if}
    </p>
  {:else}
    <div class="options">
      <label>
        <input type="checkbox" checked={wrappers === "show"} onchange={(event) => onwrappers(event.currentTarget.checked ? "show" : "hide")} />
        Unnamed groups
      </label>
      <span class="count">{shown.length} of {seen.elements.length}{#if seen.truncated} · cut short{/if}</span>
    </div>
    <!-- A tree takes focus as a whole; the picked row is its active one. -->
    <ol
      bind:this={list}
      class="rows"
      role="tree"
      tabindex="0"
      aria-label="Elements"
      aria-activedescendant={selected === null || !at.has(selected) ? undefined : `element-${selected}`}
      onkeydown={keyed}
      style:--indent="{INDENT}px"
      style:--gutter="{GUTTER}px"
    >
      {#each shown as row (row.index)}
        {@const element = seen.elements[row.index]}
        {@const named = refs?.get(row.index)}
        {#if element !== undefined}
          <!-- svelte-ignore a11y_click_events_have_key_events (the tree takes the keys for its rows) -->
          <li
            bind:this={items[row.index]}
            id="element-{row.index}"
            role="treeitem"
            aria-level={row.depth + 1}
            aria-expanded={row.branch ? row.open : undefined}
            aria-selected={row.index === selected}
            class={[
              "row",
              element.kind,
              {
                hovered: row.index === standIn(hovered ?? -1),
                selected: row.index === selected,
                trail: trail.has(row.index),
                folded: row.branch && !row.open,
              },
            ]}
            style:padding-left="{GUTTER + row.depth * INDENT}px"
            onpointerenter={() => onhover(row.index)}
            onpointerleave={() => onhover(null)}
            onclick={(event) => pressed(event, row)}
          >
            {#each row.lines as carries, level (level)}
              {#if carries}<span class="guide" style:left="{GUTTER + level * INDENT + INDENT / 2}px"></span>{/if}
            {/each}
            {#if row.depth > 0}
              <span class={["elbow", { last: row.last }]} style:left="{GUTTER + (row.depth - 1) * INDENT + INDENT / 2}px"></span>
            {/if}
            {#if row.branch}
              <span class="chevron" title={row.open ? "Fold" : "Open"}><ChevronRight size={12} /></span>
            {:else}
              <span class="leaf"><span class="dot"></span></span>
            {/if}
            {#if named !== undefined}<span class="ref" title="What the agent calls it">{named}</span>{/if}
            <span class="role">{element.roleWords}</span>
            {#if element.name !== ""}<span class="name">{oneLine(element.name)}</span>{/if}
            {#if element.value !== null}<span class="value">{oneLine(element.value)}</span>{/if}
            {#each states(element) as state (state)}<span class="state">{state}</span>{/each}
            {#if row.branch && !row.open}<span class="hidden">{element.children.length}</span>{/if}
          </li>
        {/if}
      {/each}
    </ol>
    {#if picked !== null}
      <dl class="detail">
        <dt>Role</dt>
        <dd><code>{picked.role}</code></dd>
        <dt>Index</dt>
        <dd>{picked.index} of snapshot {seen.snapshot}</dd>
        {#if refs !== null}
          {@const named = refs.get(picked.index)}
          <dt>Ref</dt>
          <dd>{#if named === undefined}none: not shown to the agent{:else}<code>{named}</code>{/if}</dd>
        {/if}
        {#if picked.identifier !== ""}
          <dt>Identifier</dt>
          <dd><code>{picked.identifier}</code></dd>
        {/if}
        <dt>Frame</dt>
        <dd>
          {#if picked.frame === null}none{:else}{Math.round(picked.frame.left)}, {Math.round(picked.frame.top)} · {Math.round(picked.frame.width)}×{Math.round(picked.frame.height)} pt{/if}
        </dd>
        {#if picked.actions.length > 0}
          <dt>Actions</dt>
          <dd>{picked.actions.map((action) => action.replace(/^AX/, "")).join(", ")}</dd>
        {/if}
      </dl>
    {/if}
  {/if}
</aside>

<style>
  .tree {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    border-left: 1px solid var(--line);
    background: var(--surface);
    font-size: 12px;
    --branch: color-mix(in srgb, var(--muted) 40%, transparent);
    /* Kind colours, as the elements layer draws them. */
    --kind-window: #9097a2;
    --kind-control: #4c8dff;
    --kind-field: #2fbf71;
    --kind-text: #b18cff;
    --kind-container: #9097a2;
    --kind-menu: #f0b429;
    --kind-other: #9097a2;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 6px 6px 12px;
  }
  .heading {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .age {
    color: var(--muted);
    white-space: nowrap;
  }
  .age.stale {
    color: var(--warning);
  }
  .options {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0 12px 6px;
    color: var(--muted);
  }
  .options label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  /* The page's inputs are form fields; this one is a plain checkbox. */
  .options input {
    width: auto;
    height: auto;
    margin: 0;
    padding: 0;
    accent-color: var(--accent-fill);
  }
  .count {
    color: var(--faint);
    white-space: nowrap;
  }
  .empty {
    margin: 0;
    padding: 12px;
    color: var(--muted);
    display: flex;
    gap: 6px;
    align-items: center;
  }
  ol {
    flex: 1;
    position: relative;
    min-height: 0;
    margin: 0;
    padding: 2px 0;
    list-style: none;
    overflow: auto;
    border-top: 1px solid var(--line);
    outline: none;
  }
  ol:focus-visible {
    box-shadow: 0 0 0 2px var(--accent-soft) inset;
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding-right: 8px;
    white-space: nowrap;
    cursor: default;
  }
  .row.hovered {
    background: var(--hover);
  }
  .row.selected {
    background: var(--accent-soft);
  }
  /* The lines that join an element to its parent, as a file browser draws them. */
  .guide,
  .elbow {
    position: absolute;
    top: 0;
    width: 1px;
    pointer-events: none;
  }
  .guide {
    bottom: 0;
    background: var(--branch);
  }
  .elbow {
    bottom: 0;
    border-left: 1px solid var(--branch);
  }
  .elbow.last {
    bottom: 50%;
  }
  .elbow::after {
    content: "";
    position: absolute;
    top: 50%;
    left: 0;
    width: calc(var(--indent) / 2 - 2px);
    border-top: 1px solid var(--branch);
  }
  .elbow.last::after {
    top: 100%;
  }
  .trail > .role,
  .trail > .name {
    color: var(--text);
  }
  .trail {
    font-weight: 500;
  }
  .chevron,
  .leaf {
    flex: none;
    display: grid;
    place-items: center;
    width: 14px;
    height: 14px;
    margin-left: 1px;
  }
  .chevron {
    border-radius: 0;
    color: var(--kind);
    transform: rotate(90deg);
    transition: transform 0.12s var(--ease);
  }
  .folded .chevron {
    transform: none;
  }
  .chevron:hover {
    background: var(--pressed);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 0;
    background: var(--kind);
  }
  .container .dot {
    background: none;
    box-shadow: 0 0 0 1px var(--kind) inset;
  }
  .hidden {
    flex: none;
    padding: 0 5px;
    border-radius: 0;
    font-size: 10px;
    line-height: 16px;
    color: var(--faint);
    box-shadow: 0 0 0 1px var(--line-strong) inset;
  }
  .window {
    --kind: var(--kind-window);
  }
  .control {
    --kind: var(--kind-control);
  }
  .field {
    --kind: var(--kind-field);
  }
  .text {
    --kind: var(--kind-text);
  }
  .container {
    --kind: var(--kind-container);
  }
  .menu {
    --kind: var(--kind-menu);
  }
  .other {
    --kind: var(--kind-other);
  }
  .role {
    color: var(--muted);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .value {
    color: var(--faint);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .value::before {
    content: "= ";
  }
  .state {
    flex: none;
    padding: 0 5px;
    border-radius: 0;
    font-size: 10px;
    line-height: 16px;
    color: var(--muted);
    background: var(--hover);
  }
  .detail {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 3px 10px;
    margin: 0;
    padding: 8px 12px 10px;
    border-top: 1px solid var(--line);
  }
  dt {
    color: var(--faint);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  code {
    font-family: var(--mono);
    font-size: 11px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .look {
    gap: 5px;
    font-size: 12px;
    white-space: nowrap;
  }
  .ref {
    flex: none;
    font: 10.5px var(--mono);
    color: var(--accent);
  }
</style>

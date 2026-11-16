<script lang="ts">
  import type { Snippet } from "svelte";
  import { X } from "@lucide/svelte";

  interface Props {
    title: string;
    /** Called when the dialog is dismissed: Escape, the close button, or a click outside. */
    onclose: () => void;
    children: Snippet;
    actions: Snippet;
  }

  let { title, onclose, children, actions }: Props = $props();
  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    dialog?.showModal();
  });
</script>

<dialog
  bind:this={dialog}
  aria-label={title}
  onclose={onclose}
  onclick={(event) => {
    if (event.target === dialog) dialog?.close();
  }}
>
  <div class="box">
    <header>
      <h2>{title}</h2>
      <button class="ghost icon" aria-label="Close" onclick={() => dialog?.close()}><X size={16} /></button>
    </header>
    <div class="body">{@render children()}</div>
    <footer>{@render actions()}</footer>
  </div>
</dialog>

<style>
  dialog {
    padding: 0;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-lg);
    background: var(--overlay);
    color: var(--text);
    box-shadow: var(--shadow);
    width: min(440px, calc(100vw - 32px));
  }
  dialog::backdrop {
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(3px);
  }
  dialog[open] {
    animation: rise 0.18s var(--ease);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px) scale(0.98);
    }
  }
  .box {
    display: grid;
    gap: 16px;
    padding: 18px 18px 16px;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .body {
    display: grid;
    gap: 14px;
    font-size: 14px;
    color: var(--muted);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>

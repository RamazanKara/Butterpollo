<script lang="ts">
  import { dismiss, toasts } from '../lib/feedback.svelte';
  import Icon from './Icon.svelte';
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast {toast.tone}">
      <span>{toast.message}</span>
      <button aria-label="Dismiss" onclick={() => dismiss(toast.id)}><Icon name="x" size={14} /></button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--space-4);
    bottom: var(--space-4);
    display: grid;
    gap: var(--space-2);
    z-index: 50;
    width: min(380px, calc(100vw - 32px));
  }
  .toast {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
    justify-content: space-between;
    padding: 11px 12px 11px 14px;
    border-radius: var(--radius);
    background: var(--panel-raised);
    border: 1px solid var(--line);
    border-left: 3px solid var(--ink-2);
    box-shadow: var(--shadow-pop);
    font-size: var(--text-sm);
  }
  .ok {
    border-left-color: var(--ok);
  }
  .warn {
    border-left-color: var(--warn);
  }
  .danger {
    border-left-color: var(--danger);
  }
  .toast > span {
    min-width: 0;
  }
  button {
    flex: none;
    border: 0;
    background: none;
    color: var(--muted);
    cursor: pointer;
    padding: 2px;
  }
</style>

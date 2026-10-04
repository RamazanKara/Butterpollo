<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    tone = 'neutral',
    children,
  }: { tone?: 'neutral' | 'ok' | 'warn' | 'danger' | 'accent' | 'live'; children: Snippet } = $props();
</script>

<span class="badge {tone}">
  {#if tone === 'live'}<span class="dot" aria-hidden="true"></span>{/if}
  {@render children()}
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    font-weight: 600;
    line-height: 1.6;
    white-space: nowrap;
    background: var(--sunken);
    color: var(--ink-2);
    border: 1px solid var(--line);
  }
  .ok,
  .live {
    background: var(--ok-soft);
    color: var(--ok);
    border-color: transparent;
  }
  .warn {
    background: var(--warn-soft);
    color: var(--warn);
    border-color: transparent;
  }
  .danger {
    background: var(--danger-soft);
    color: var(--danger);
    border-color: transparent;
  }
  .accent {
    background: var(--accent-soft);
    color: var(--ink);
    border-color: transparent;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    animation: beat 1.6s ease-in-out infinite;
  }
  @keyframes beat {
    50% {
      opacity: 0.35;
    }
  }
</style>

<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    label,
    hint,
    error,
    id,
    wide = false,
    children,
  }: {
    label: string;
    hint?: string;
    error?: string;
    /** The id of the control, so the label is clickable. */
    id?: string;
    /** Span both columns in a two-column form. */
    wide?: boolean;
    children: Snippet;
  } = $props();
</script>

<div class="field" class:wide>
  <label for={id}>{label}</label>
  {@render children()}
  {#if error}
    <p class="error">{error}</p>
  {:else if hint}
    <p class="hint">{hint}</p>
  {/if}
</div>

<style>
  .field {
    display: grid;
    gap: 6px;
    align-content: start;
    min-width: 0;
  }
  .wide {
    grid-column: 1 / -1;
  }
  label {
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .hint,
  .error {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
  }
  .error {
    color: var(--danger);
  }
</style>

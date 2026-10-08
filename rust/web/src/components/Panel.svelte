<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title,
    description,
    actions,
    flush = false,
    children,
  }: {
    title?: string;
    description?: string;
    actions?: Snippet;
    /** Content reaches the panel's edges (tables, lists). */
    flush?: boolean;
    children: Snippet;
  } = $props();
</script>

<section class="panel">
  {#if title || actions}
    <header>
      <div>
        {#if title}<h2>{title}</h2>{/if}
        {#if description}<p class="muted">{description}</p>{/if}
      </div>
      {#if actions}<div class="actions">{@render actions()}</div>{/if}
    </header>
  {/if}
  <div class:body={!flush}>
    {@render children()}
  </div>
</section>

<style>
  .panel {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
    min-width: 0;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: var(--space-4);
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--line);
  }
  header > div {
    min-width: 0;
  }
  header p {
    font-size: var(--text-sm);
    margin-top: 3px;
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .body {
    padding: var(--space-5);
  }
  @media (max-width: 520px) {
    header,
    .body {
      padding: var(--space-4);
    }
  }
</style>

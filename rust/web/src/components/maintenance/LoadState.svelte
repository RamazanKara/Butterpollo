<script lang="ts">
  let { error, what }: { error: string; what: string } = $props();
</script>

{#if error}
  <p class="notice danger" role="alert">Could not load {what}: {error}</p>
{:else}
  <p class="muted loading">Loading {what}…</p>
{/if}

<style>
  .loading {
    font-size: var(--text-sm);
  }
</style>

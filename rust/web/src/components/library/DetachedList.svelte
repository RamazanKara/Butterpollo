<script lang="ts">
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import { textRow, type TextRow } from './draft.svelte';

  let { rows = $bindable(), id }: { rows: TextRow[]; id: string } = $props();
</script>

<fieldset class="list" aria-describedby="{id}-hint">
  <legend>Detached commands</legend>
  <p class="hint" id="{id}-hint">Run independently of the app and stay open when it closes, such as game launchers.</p>
  {#each rows as row, index (row.key)}
    <div class="item">
      <input
        class="input mono"
        aria-label="Detached command {index + 1}"
        autocomplete="off"
        spellcheck="false"
        bind:value={row.value}
      />
      <button
        type="button"
        class="remove"
        title="Remove"
        aria-label="Remove detached command {index + 1}"
        onclick={() => rows.splice(index, 1)}
      >
        <Icon name="x" size={15} />
      </button>
    </div>
  {/each}
  <div>
    <Button size="sm" icon="plus" onclick={() => rows.push(textRow())}>Add detached command</Button>
  </div>
</fieldset>

<style>
  .list {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 4px;
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
    margin-top: -4px;
  }
  .item {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--space-2);
    align-items: center;
  }
  .remove {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: none;
    color: var(--muted);
    cursor: pointer;
  }
  .remove:hover {
    background: var(--danger-soft);
    color: var(--danger);
  }
</style>

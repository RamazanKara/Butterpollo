<script lang="ts">
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import { commandRow, type CommandRow } from './device';

  let { rows = $bindable(), label, hint }: { rows: CommandRow[]; label: string; hint: string } = $props();

  // The host refuses longer lists.
  const LIMIT = 64;
</script>

<fieldset class="commands">
  <legend>{label}</legend>
  <p class="hint">{hint}</p>
  {#each rows as row, index (row.key)}
    <div class="command">
      <input
        class="input mono"
        aria-label="{label}, command {index + 1}"
        autocomplete="off"
        spellcheck="false"
        bind:value={row.cmd}
      />
      <label class="check">
        <input type="checkbox" bind:checked={row.elevated} />
        As administrator
      </label>
      <button
        type="button"
        class="remove"
        aria-label="Remove {label.toLowerCase()} command {index + 1}"
        title="Remove"
        onclick={() => rows.splice(index, 1)}
      >
        <Icon name="x" size={15} />
      </button>
    </div>
  {:else}
    <p class="muted none">None.</p>
  {/each}
  <div>
    <Button size="sm" icon="plus" disabled={rows.length >= LIMIT} onclick={() => rows.push(commandRow())}>
      Add command
    </Button>
  </div>
</fieldset>

<style>
  .commands {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  legend {
    padding: 0;
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .hint,
  .none {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .command {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center;
    gap: var(--space-2) var(--space-3);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    white-space: nowrap;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--ink-2);
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
  @media (max-width: 520px) {
    .command {
      grid-template-columns: minmax(0, 1fr) auto;
      padding-bottom: var(--space-2);
      border-bottom: 1px solid var(--line);
    }
    .command .input {
      grid-column: 1 / -1;
    }
  }
</style>

<script lang="ts">
  import { tick } from 'svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import { prepRow, type PrepRow } from './draft.svelte';

  let { rows = $bindable(), id }: { rows: PrepRow[]; id: string } = $props();

  let list = $state<HTMLElement>();

  async function focus(key: number, selector: string) {
    await tick();
    const row = list?.querySelector(`[data-key="${key}"]`);
    const target = row?.querySelector<HTMLElement>(selector);
    (target && !(target as HTMLButtonElement).disabled ? target : row?.querySelector<HTMLElement>('input'))?.focus();
  }

  function move(index: number, delta: -1 | 1) {
    const target = index + delta;
    if (target < 0 || target >= rows.length) return;
    const [row] = rows.splice(index, 1);
    if (!row) return;
    rows.splice(target, 0, row);
    // Moving the row's elements drops focus; put it back on the same button.
    void focus(row.key, delta < 0 ? '[data-move="up"]' : '[data-move="down"]');
  }

  function add() {
    const row = prepRow();
    rows.push(row);
    void focus(row.key, 'input');
  }
</script>

<div class="prep" bind:this={list}>
  {#if rows.length}
    <div class="head" aria-hidden="true">
      <span>Do</span>
      <span>Undo</span>
    </div>
    <ol class="rows">
      {#each rows as row, index (row.key)}
        <li class="row-item" data-key={row.key}>
          <div class="cell">
            <label class="cell-label" for="{id}-do-{row.key}">Do, command {index + 1}</label>
            <input
              id="{id}-do-{row.key}"
              class="input mono"
              autocomplete="off"
              spellcheck="false"
              placeholder="Before the app starts"
              bind:value={row.do}
            />
          </div>
          <div class="cell">
            <label class="cell-label" for="{id}-undo-{row.key}">Undo, command {index + 1}</label>
            <input
              id="{id}-undo-{row.key}"
              class="input mono"
              autocomplete="off"
              spellcheck="false"
              placeholder="After it closes"
              bind:value={row.undo}
            />
          </div>
          <div class="controls">
            <label class="check">
              <input type="checkbox" bind:checked={row.elevated} />
              As administrator
            </label>
            <span class="buttons">
              <button
                type="button"
                class="icon-button"
                data-move="up"
                title="Move up"
                aria-label="Move command {index + 1} up"
                disabled={index === 0}
                onclick={() => move(index, -1)}
              >
                <span class="up"><Icon name="chevron" size={15} /></span>
              </button>
              <button
                type="button"
                class="icon-button"
                data-move="down"
                title="Move down"
                aria-label="Move command {index + 1} down"
                disabled={index === rows.length - 1}
                onclick={() => move(index, 1)}
              >
                <span class="down"><Icon name="chevron" size={15} /></span>
              </button>
              <button
                type="button"
                class="icon-button remove"
                title="Remove"
                aria-label="Remove command {index + 1}"
                onclick={() => rows.splice(index, 1)}
              >
                <Icon name="x" size={15} />
              </button>
            </span>
          </div>
        </li>
      {/each}
    </ol>
  {:else}
    <p class="muted none">None.</p>
  {/if}
  <div>
    <Button size="sm" icon="plus" onclick={add}>Add command</Button>
  </div>
</div>

<style>
  .prep {
    display: grid;
    gap: var(--space-2);
    container-type: inline-size;
    min-width: 0;
  }
  .head,
  .row-item {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
    gap: var(--space-2) var(--space-3);
    align-items: center;
  }
  .head {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
  }
  .head::after {
    content: '';
  }
  .rows {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .cell {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  /* Column headings label the inputs on wide screens; each row says it on narrow ones. */
  .cell-label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: var(--space-3);
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
    margin: 0;
    accent-color: var(--ink-2);
  }
  .buttons {
    display: flex;
  }
  .icon-button {
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
  .icon-button:hover:not(:disabled) {
    background: var(--sunken);
    color: var(--ink);
  }
  .icon-button:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .remove:hover:not(:disabled) {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .up,
  .down {
    display: inline-flex;
  }
  .up {
    transform: rotate(-90deg);
  }
  .down {
    transform: rotate(90deg);
  }
  .none {
    font-size: var(--text-sm);
  }
  @container (max-width: 620px) {
    .head {
      display: none;
    }
    .row-item {
      grid-template-columns: minmax(0, 1fr);
      padding-bottom: var(--space-3);
      border-bottom: 1px solid var(--line);
    }
    .cell-label {
      position: static;
      width: auto;
      height: auto;
      overflow: visible;
      clip: auto;
      font-size: var(--text-xs);
      font-weight: 600;
      color: var(--muted);
    }
    .controls {
      justify-content: space-between;
    }
  }
</style>

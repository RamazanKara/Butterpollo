<script lang="ts">
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import Icon from '../Icon.svelte';
  import type { Config } from '../../lib/api';
  import { has, NOT_SETTINGS, toText, type Edits } from './values';

  let {
    keys,
    stored,
    edits,
    changes,
    isDescribed,
    onedit,
    onremove,
    onundo,
    adding = true,
  }: {
    /** Keys to list: stored or added here, not described by the schema. */
    keys: string[];
    stored: Config;
    edits: Edits;
    changes: Config;
    /** The schema has a setting for this key. */
    isDescribed: (key: string) => boolean;
    onedit: (key: string, value: string) => void;
    onremove: (key: string) => void;
    onundo: (key: string) => void;
    /** Show the form for adding a key (not while searching). */
    adding?: boolean;
  } = $props();

  const uid = $props.id();

  // A key marked for removal still shows what it held.
  const text = (key: string) => toText(has(edits, key) && edits[key] !== null ? edits[key] : stored[key]);

  let newKey = $state('');
  let newValue = $state('');
  let tried = $state(false);

  const keyError = $derived.by(() => {
    const key = newKey.trim();
    if (!key) return 'Enter a key.';
    if (!/^[A-Za-z0-9_]+$/.test(key)) return 'Use letters, digits and underscores only.';
    if (NOT_SETTINGS.has(key)) return 'This key is not a setting.';
    if (isDescribed(key)) return 'This key has its own setting. Search for it above.';
    if (keys.includes(key) || has(stored, key)) return 'This key is already listed.';
    return '';
  });
  const valueError = $derived(newValue.trim() ? '' : 'Enter a value.');

  function add(event: SubmitEvent) {
    event.preventDefault();
    tried = true;
    if (keyError || valueError) return;
    onedit(newKey.trim(), newValue.trim());
    newKey = '';
    newValue = '';
    tried = false;
  }
</script>

<section class="unknown" aria-labelledby="{uid}-title">
  <div class="intro">
    <h3 id="{uid}-title">Other keys</h3>
    <p>Keys in the settings file that this console does not describe. Values are saved exactly as typed.</p>
  </div>

  {#if keys.length}
    <ul>
      {#each keys as key (key)}
        {@const removed = edits[key] === null}
        {@const marked = has(changes, key)}
        <li class:marked>
          <label class="key" for="{uid}-{key}"><code>{key}</code></label>
          <div class="value">
            <input
              id="{uid}-{key}"
              class="input mono"
              autocomplete="off"
              spellcheck="false"
              disabled={removed}
              value={text(key)}
              oninput={(event) => onedit(key, event.currentTarget.value)}
            />
            {#if removed}
              <Button size="sm" onclick={() => onundo(key)}>
                Undo<span class="visually-hidden">{` removing ${key}`}</span>
              </Button>
            {:else}
              <button type="button" class="remove" aria-label="Remove {key}" title="Remove" onclick={() => onremove(key)}>
                <Icon name="trash" size={15} />
              </button>
            {/if}
          </div>
          {#if removed}
            <p class="state">Removed when saved</p>
          {:else if marked}
            <p class="state">
              <span class="changed">{has(stored, key) ? 'Changed' : 'New'}</span>
              <button type="button" class="link" onclick={() => onundo(key)}>
                Undo<span class="visually-hidden">{` ${key}`}</span>
              </button>
            </p>
          {/if}
        </li>
      {/each}
    </ul>
  {:else if adding}
    <p class="none">The settings file has no other keys.</p>
  {/if}

  {#if adding}
    <form class="add" onsubmit={add} aria-label="Add a key">
      <Field label="Key" id="{uid}-new-key" error={tried && keyError ? keyError : undefined}>
        <input id="{uid}-new-key" class="input mono" autocomplete="off" spellcheck="false" bind:value={newKey} />
      </Field>
      <Field label="Value" id="{uid}-new-value" error={tried && valueError ? valueError : undefined}>
        <input id="{uid}-new-value" class="input mono" autocomplete="off" spellcheck="false" bind:value={newValue} />
      </Field>
      <div class="add-action">
        <Button type="submit" size="sm" icon="plus">Add key</Button>
      </div>
    </form>
  {/if}
</section>

<style>
  .unknown {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
    border-top: 1px solid var(--line);
    min-width: 0;
  }
  .unknown:first-child {
    border-top: 0;
  }
  .intro {
    display: grid;
    gap: 2px;
  }
  h3 {
    font-size: var(--text-sm);
  }
  .intro p,
  .none,
  .state {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  ul {
    display: grid;
    margin: 0;
    padding: 0;
    list-style: none;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: clip;
  }
  li {
    display: grid;
    grid-template-columns: minmax(120px, 220px) minmax(0, 1fr);
    align-items: center;
    gap: 4px var(--space-3);
    padding: var(--space-2) var(--space-3);
  }
  li + li {
    border-top: 1px solid var(--line);
  }
  li.marked {
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .key {
    min-width: 0;
    overflow-wrap: anywhere;
    color: var(--ink-2);
    font-size: var(--text-sm);
  }
  .value {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .value .input:disabled {
    text-decoration: line-through;
  }
  .state {
    grid-column: 2;
    display: flex;
    gap: var(--space-3);
  }
  .changed {
    color: var(--ink);
    font-weight: 600;
  }
  .remove {
    flex: none;
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
  .link {
    border: 0;
    padding: 0;
    background: none;
    color: var(--ink-2);
    font-size: var(--text-xs);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .add {
    display: grid;
    grid-template-columns: minmax(120px, 220px) minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-2) var(--space-3);
  }
  .add-action {
    /* Line up with the inputs, below the labels. */
    padding-top: 24px;
  }
  @media (max-width: 620px) {
    li,
    .add {
      grid-template-columns: minmax(0, 1fr);
    }
    .state {
      grid-column: 1;
    }
    .add-action {
      padding-top: 0;
    }
  }
  @media (max-width: 520px) {
    .unknown {
      padding: var(--space-4);
    }
  }
</style>

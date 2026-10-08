<script lang="ts">
  import type { Snippet } from 'svelte';
  import Button from './Button.svelte';
  import Icon from './Icon.svelte';
  import { api, type BrowseEntry, type BrowseListing, type BrowseType } from '../lib/api';
  import { startPath } from '../lib/paths';

  let {
    kind,
    value,
    onpick,
    children,
  }: {
    /** What can be picked: a program, any file, or a folder. */
    kind: BrowseType;
    /** The field's current value, where browsing starts. */
    value: string;
    onpick: (path: string) => void;
    /** The text field the picker fills. */
    children: Snippet;
  } = $props();

  const uid = $props.id();
  let open = $state(false);
  let listing = $state<BrowseListing | null>(null);
  let loading = $state(false);
  let error = $state('');
  let toggle = $state<HTMLDivElement>();
  /** Only the latest listing asked for is shown. */
  let request = 0;

  const what = $derived(kind === 'directory' ? 'folder' : kind === 'executable' ? 'program' : 'file');
  const atDrives = $derived(listing !== null && listing.path === '');

  async function load(path: string, fallBackToDrives = false) {
    const id = ++request;
    loading = true;
    error = '';
    try {
      const result = await api.browse(path, kind);
      if (id === request) listing = result;
    } catch (failure) {
      if (id !== request) return;
      // A value that no longer exists, such as a removed drive.
      if (fallBackToDrives && path) return load('');
      error = failure instanceof Error ? failure.message : String(failure);
    } finally {
      if (id === request) loading = false;
    }
  }

  function show() {
    open = !open;
    if (open) void load(startPath(value), true);
  }

  function close() {
    open = false;
    toggle?.querySelector('button')?.focus();
  }

  function pick(path: string) {
    onpick(path);
    close();
  }

  // Escape inside the browser closes it, like a menu.
  function closeOnEscape(node: HTMLElement) {
    const keydown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      close();
    };
    node.addEventListener('keydown', keydown);
    return () => node.removeEventListener('keydown', keydown);
  }

  function enter(entry: BrowseEntry) {
    if (entry.type === 'directory') void load(entry.path);
    else pick(entry.path);
  }
</script>

<div class="path-picker">
  <div class="line">
    {@render children()}
    <div bind:this={toggle}>
      <Button icon="folder" onclick={show}>{open ? 'Hide' : 'Browse'}</Button>
    </div>
  </div>

  {#if open}
    <div
      class="browser"
      role="group"
      aria-label="Choose a {what} on the host"
      {@attach closeOnEscape}
    >
      <div class="bar">
        <Button
          size="sm"
          variant="ghost"
          icon="up"
          title="Up one folder"
          disabled={loading || listing === null || atDrives}
          onclick={() => listing && void load(listing.parent)}
        >
          Up
        </Button>
        <span class="mono where" id="{uid}-where">{listing && !atDrives ? listing.path : 'This PC'}</span>
        {#if kind === 'directory'}
          <Button
            size="sm"
            icon="check"
            disabled={loading || listing === null || atDrives}
            onclick={() => listing && pick(listing.path)}
          >
            Use this folder
          </Button>
        {/if}
      </div>
      <div aria-live="polite">
        {#if loading && listing === null}
          <p class="muted state">Loading…</p>
        {:else if error}
          <p class="notice danger">Listing the folder failed: {error}</p>
        {:else if listing && listing.entries.length === 0}
          <p class="muted state">{kind === 'directory' ? 'No folders here.' : `No folders or ${what}s here.`}</p>
        {/if}
      </div>
      {#if listing && listing.entries.length > 0}
        <ul class="entries" aria-labelledby="{uid}-where" aria-busy={loading}>
          {#each listing.entries as entry (entry.path)}
            <li>
              <button type="button" class="entry" disabled={loading} onclick={() => enter(entry)}>
                <span class="kind" class:file={entry.type === 'file'}>
                  <Icon name={atDrives ? 'drive' : entry.type === 'directory' ? 'folder' : 'file'} size={16} />
                </span>
                <span class="name">{entry.name}</span>
                {#if entry.type === 'file'}<span class="visually-hidden">Use this {what}</span>{/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .path-picker {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
  }
  .line {
    display: flex;
    gap: var(--space-2);
    align-items: stretch;
    min-width: 0;
  }
  .line > div {
    display: flex;
  }
  .browser {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--sunken);
    min-width: 0;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .where {
    flex: 1 1 160px;
    min-width: 0;
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .state {
    font-size: var(--text-sm);
  }
  .entries {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 280px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
  }
  .entries li + li {
    border-top: 1px solid var(--line);
  }
  .entry {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    padding: 7px var(--space-3);
    border: 0;
    background: none;
    color: var(--ink);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .entry:hover:not(:disabled) {
    background: var(--sunken);
  }
  .entry:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: -2px;
  }
  .entry:disabled {
    cursor: default;
    color: var(--muted);
  }
  .kind {
    color: var(--accent);
  }
  .kind.file {
    color: var(--muted);
  }
  .name {
    min-width: 0;
    overflow-wrap: anywhere;
  }
</style>

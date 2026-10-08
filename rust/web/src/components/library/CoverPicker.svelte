<script lang="ts">
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import AppCover from './AppCover.svelte';
  import { api } from '../../lib/api';
  import { failed } from '../../lib/feedback.svelte';
  import { coverPath } from './app';
  import { coverFromFile, searchCovers, type CoverCandidate } from './covers';
  import type { AppDraft } from './draft.svelte';

  let { draft }: { draft: AppDraft } = $props();

  const uid = $props.id();
  let open = $state(false);
  let query = $state('');
  let results = $state<CoverCandidate[] | null>(null);
  let searchedFor = $state('');
  let searching = $state(false);
  let searchError = $state('');
  /** Key of the cover being stored, or 'upload'. */
  let storing = $state('');
  let fileInput = $state<HTMLInputElement>();
  /** An image chosen in this visit, shown until the host serves it. */
  let chosen = $state<{ path: string; src: string } | null>(null);
  let controller: AbortController | null = null;

  const path = $derived(draft.cover());
  const savedPath = $derived(coverPath(draft.saved));
  const preview = $derived(chosen && chosen.path === path ? chosen.src : undefined);
  const name = $derived(draft.app.name.trim());
  const playnite = $derived(typeof draft.app['playnite-id'] === 'string' && draft.app['playnite-id'] !== '');

  // Search 300 ms after the last keystroke.
  $effect(() => {
    if (!open) return;
    const text = query.trim();
    const timer = setTimeout(() => void search(text), 300);
    return () => clearTimeout(timer);
  });
  $effect(() => () => controller?.abort());

  const reason = (error: unknown) => (error instanceof Error ? error.message : String(error));

  async function search(text: string) {
    controller?.abort();
    const current = new AbortController();
    controller = current;
    if (!text) {
      results = null;
      searching = false;
      searchError = '';
      return;
    }
    searching = true;
    searchError = '';
    try {
      const found = await searchCovers(text, current.signal);
      if (current.signal.aborted) return;
      results = found;
      searchedFor = text;
    } catch (error) {
      if (current.signal.aborted) return;
      results = null;
      searchError = reason(error);
    } finally {
      if (controller === current) searching = false;
    }
  }

  function toggleFinder() {
    open = !open;
    if (open && !query) query = name;
  }

  async function choose(candidate: CoverCandidate) {
    storing = candidate.key;
    try {
      const stored = await api.apps.uploadCover(candidate.key, { url: candidate.saveUrl });
      chosen = { path: stored.path, src: candidate.thumb };
      draft.setCover(stored.path);
      open = false;
    } catch (error) {
      failed('Getting the cover failed', error);
    } finally {
      storing = '';
    }
  }

  async function upload(event: Event & { currentTarget: HTMLInputElement }) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // Choosing the same file again still fires a change.
    input.value = '';
    if (!file) return;
    storing = 'upload';
    try {
      const image = await coverFromFile(file);
      const stored = await api.apps.uploadCover(`upload_${Date.now()}`, { data: image.data });
      chosen = { path: stored.path, src: image.preview };
      draft.setCover(stored.path);
    } catch (error) {
      failed('Uploading the image failed', error);
    } finally {
      storing = '';
    }
  }

  function remove() {
    chosen = null;
    draft.setCover('');
  }
</script>

<div class="picker">
  <div class="preview">
    <AppCover
      name={draft.app.name}
      uuid={draft.saved.uuid}
      path={path && path === savedPath ? savedPath : ''}
      src={preview}
      alt={name ? `Cover of ${name}` : 'Cover'}
    />
  </div>
  {#if path !== savedPath}
    <p class="change">
      {path
        ? `New cover. Save to keep it${playnite ? ' and to set it in Playnite' : ''}.`
        : 'Cover removed. Save to keep the change.'}
    </p>
  {:else if !path}
    <p class="muted change">No cover. Devices show the host’s default image.</p>
  {/if}

  <div class="actions">
    <Button size="sm" icon="search" disabled={storing !== ''} onclick={toggleFinder}>
      {open ? 'Hide search' : 'Find cover'}
    </Button>
    <Button size="sm" icon="upload" busy={storing === 'upload'} disabled={storing !== ''} onclick={() => fileInput?.click()}>
      Upload image
    </Button>
    <Button size="sm" variant="ghost" disabled={!path || storing !== ''} onclick={remove}>Remove cover</Button>
  </div>
  <input
    bind:this={fileInput}
    class="visually-hidden"
    type="file"
    accept="image/*"
    tabindex="-1"
    aria-label="Upload image"
    onchange={upload}
  />

  {#if open}
    <div class="finder">
      <Field label="Game name" id="{uid}-query" hint="Covers from IGDB, found through LizardByte’s GameDB.">
        <input
          id="{uid}-query"
          class="input"
          type="search"
          autocomplete="off"
          spellcheck="false"
          bind:value={query}
          onkeydown={(event) => {
            // Enter searches now instead of saving the app.
            if (event.key === 'Enter') {
              event.preventDefault();
              void search(query.trim());
            }
          }}
        />
      </Field>
      <div aria-live="polite">
        {#if searching}
          <p class="muted state">Searching…</p>
        {:else if searchError}
          <p class="notice danger">Searching GameDB failed: {searchError}</p>
        {:else if results && results.length === 0}
          <p class="muted state">No covers found for “{searchedFor}”.</p>
        {/if}
      </div>
      {#if results && results.length > 0}
        <ul class="results" aria-label="Covers found">
          {#each results as candidate (candidate.key)}
            <li>
              <button
                type="button"
                class="candidate"
                disabled={storing !== ''}
                aria-busy={storing === candidate.key}
                title={candidate.name}
                onclick={() => choose(candidate)}
              >
                <span class="thumb">
                  <img src={candidate.thumb} alt="" loading="lazy" />
                  {#if storing === candidate.key}<span class="spinner" aria-hidden="true"></span>{/if}
                </span>
                <span class="candidate-name">{candidate.name}</span>
                <span class="visually-hidden">Use this cover</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .picker {
    display: grid;
    gap: var(--space-3);
    min-width: 0;
  }
  .preview {
    width: min(100%, 200px);
  }
  .change {
    font-size: var(--text-xs);
    line-height: 1.45;
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .finder {
    display: grid;
    gap: var(--space-3);
    padding-top: var(--space-3);
    border-top: 1px solid var(--line);
  }
  .state {
    font-size: var(--text-sm);
  }
  .results {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(72px, 1fr));
    gap: var(--space-3) var(--space-2);
    max-height: 420px;
    overflow-y: auto;
    margin: 0;
    padding: 2px;
    list-style: none;
  }
  .candidate {
    display: grid;
    gap: 4px;
    width: 100%;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
    cursor: pointer;
  }
  .candidate:disabled {
    cursor: default;
  }
  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 2 / 3;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--sunken);
  }
  .candidate:hover:not(:disabled) .thumb {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .candidate:disabled:not([aria-busy='true']) .thumb img {
    opacity: 0.6;
  }
  .spinner {
    position: absolute;
    inset: 0;
    margin: auto;
    width: 20px;
    height: 20px;
    border: 2px solid var(--panel);
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .candidate-name {
    font-size: var(--text-xs);
    line-height: 1.3;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
</style>

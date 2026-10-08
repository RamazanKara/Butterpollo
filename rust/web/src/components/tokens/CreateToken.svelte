<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import Panel from '../Panel.svelte';
  import { api, type TokenScope } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import { displayPath } from './scopes';

  let { oncreated }: { oncreated: () => Promise<void> } = $props();

  /** The host accepts at most this many routes per token. */
  const MAX_ROUTES = 64;

  let routes = $state<TokenScope[] | null>(null);
  let loadError = $state('');
  let query = $state('');
  let busy = $state(false);
  let secret = $state<string | null>(null);
  const chosen = new SvelteSet<string>();

  const key = (method: string, path: string) => `${method} ${path}`;

  const shown = $derived.by(() => {
    const needle = query.trim().toLowerCase();
    if (!routes || !needle) return routes ?? [];
    return routes.filter((route) =>
      `${route.methods.join(' ')} ${route.path} ${displayPath(route.path)}`.toLowerCase().includes(needle),
    );
  });
  const scopes = $derived(
    (routes ?? [])
      .map((route) => ({ path: route.path, methods: route.methods.filter((method) => chosen.has(key(method, route.path))) }))
      .filter((scope) => scope.methods.length > 0),
  );
  const tooMany = $derived(scopes.length > MAX_ROUTES);

  onMount(async () => {
    try {
      routes = (await api.tokens.routes()).routes;
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
    }
  });

  function toggle(method: string, path: string, on: boolean) {
    if (on) chosen.add(key(method, path));
    else chosen.delete(key(method, path));
  }

  async function create() {
    if (!scopes.length || tooMany) return;
    busy = true;
    try {
      secret = (await api.tokens.create(scopes)).token;
      chosen.clear();
      query = '';
      await oncreated();
    } catch (error) {
      failed('Creating the token failed', error);
    } finally {
      busy = false;
    }
  }

  async function copy() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      notify('Token copied.', 'ok');
    } catch (error) {
      failed('Copying failed; select the token and copy it by hand', error);
    }
  }
</script>

<Panel title="Create a token" description="Select the API paths the script needs and the request methods allowed on each path.">
  {#if secret}
    <div class="stack issued">
      <p class="notice warn" role="status">
        <Icon name="alert" />
        <span>Copy this token now. It is not shown again; if you lose it, revoke it and create another.</span>
      </p>
      <div class="secret">
        <code class="mono" aria-label="New token">{secret}</code>
        <Button size="sm" icon="copy" onclick={copy}>Copy</Button>
      </div>
      <div class="row">
        <Button onclick={() => (secret = null)}>Done</Button>
      </div>
    </div>
  {:else if routes}
    <div class="stack">
      <div class="search">
        <label class="visually-hidden" for="route-search">Search API paths</label>
        <span class="search-icon" aria-hidden="true"><Icon name="search" size={15} /></span>
        <input
          id="route-search"
          class="input"
          type="search"
          placeholder="Search API paths"
          autocomplete="off"
          spellcheck="false"
          bind:value={query}
        />
      </div>
      <ul class="routes" aria-label="API paths">
        {#each shown as route (route.path)}
          <li class:picked={route.methods.some((method) => chosen.has(key(method, route.path)))}>
            <span class="mono path">{displayPath(route.path)}</span>
            <span class="methods">
              {#each route.methods as method (method)}
                <label class="method">
                  <input
                    type="checkbox"
                    aria-label="{method} {displayPath(route.path)}"
                    checked={chosen.has(key(method, route.path))}
                    onchange={(event) => toggle(method, route.path, event.currentTarget.checked)}
                  />
                  <span class="mono">{method}</span>
                </label>
              {/each}
            </span>
          </li>
        {:else}
          <li class="none muted">No API paths match “{query}”.</li>
        {/each}
      </ul>
      <div class="footer">
        <span class="muted count">
          {#if tooMany}
            <span class="over">{scopes.length} paths selected; a token can have at most {MAX_ROUTES}.</span>
          {:else if chosen.size}
            {chosen.size} {chosen.size === 1 ? 'permission' : 'permissions'} on {scopes.length}
            {scopes.length === 1 ? 'path' : 'paths'}
          {:else}
            No permissions selected
          {/if}
        </span>
        <div class="row">
          {#if chosen.size}<Button variant="ghost" onclick={() => chosen.clear()}>Clear selection</Button>{/if}
          <Button variant="primary" icon="key" {busy} disabled={!scopes.length || tooMany} onclick={create}>Create token</Button>
        </div>
      </div>
    </div>
  {:else if loadError}
    <p class="notice danger" role="alert">Could not load the API paths: {loadError}</p>
  {:else}
    <p class="muted loading">Loading API paths…</p>
  {/if}
</Panel>

<style>
  .issued {
    gap: var(--space-4);
  }
  .secret {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    background: var(--sunken);
  }
  .secret code {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
    user-select: all;
    font-size: var(--text-sm);
  }
  .search {
    position: relative;
    max-width: 360px;
  }
  .search .input {
    padding-left: 30px;
  }
  .search-icon {
    position: absolute;
    left: 9px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--muted);
    pointer-events: none;
  }
  .routes {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    max-height: 420px;
    overflow: auto;
  }
  .routes li {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px var(--space-4);
    padding: 7px var(--space-3);
    border-bottom: 1px solid var(--line);
  }
  .routes li:last-child {
    border-bottom: 0;
  }
  .routes li.picked {
    background: var(--sunken);
  }
  .routes .none {
    font-size: var(--text-sm);
  }
  .path {
    /* Grows to a fixed column on wide screens so the methods line up. */
    flex: 1 1 auto;
    max-width: 320px;
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .methods {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  .method {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .method input {
    margin: 0;
    accent-color: var(--accent);
  }
  .footer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .count {
    font-size: var(--text-sm);
  }
  .over {
    color: var(--danger);
  }
  .loading {
    font-size: var(--text-sm);
  }
</style>

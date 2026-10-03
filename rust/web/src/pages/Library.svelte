<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../components/Button.svelte';
  import EmptyState from '../components/EmptyState.svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import Panel from '../components/Panel.svelte';
  import AppEditor from '../components/library/AppEditor.svelte';
  import LibraryGrid from '../components/library/LibraryGrid.svelte';
  import SteamSource from '../components/library/SteamSource.svelte';
  import PlayniteSource from '../components/library/PlayniteSource.svelte';
  import { api, type App, type SessionStatus } from '../lib/api';
  import { failed } from '../lib/feedback.svelte';
  import { poll } from '../lib/format';
  import { match, navigate } from '../lib/router.svelte';

  let apps = $state<App[] | null>(null);
  let loadError = $state('');
  let status = $state<SessionStatus | null>(null);
  let latestLoad = 0;

  /** null on the grid, 'new' for a new app, otherwise the app's uuid. */
  const editing = $derived(match('/library/new') ? 'new' : (match('/library/:uuid')?.uuid ?? null));

  const reason = (error: unknown) => (error instanceof Error ? error.message : String(error));

  async function load() {
    // A slow reload must not put an older list back.
    const id = ++latestLoad;
    try {
      const list = (await api.apps.list()).apps ?? [];
      if (id !== latestLoad) return;
      apps = list;
      loadError = '';
    } catch (error) {
      if (id === latestLoad) loadError = reason(error);
    }
  }

  async function refreshStatus() {
    try {
      status = await api.sessions.status();
    } catch {
      // The Running badge keeps its last state.
    }
  }

  onMount(() => {
    void load();
    return poll(refreshStatus, 5000);
  });

  async function reorder(next: App[]) {
    apps = next;
    try {
      await api.apps.reorder(next.flatMap((app) => (app.uuid ? [app.uuid] : [])));
    } catch (error) {
      failed('Saving the order failed', error);
      await load();
    }
  }

  const current = $derived(editing && editing !== 'new' ? apps?.find((app) => app.uuid === editing) : undefined);
  // Moving needs every app's uuid; the host puts apps it is not given at the end.
  const position = $derived.by(() => {
    if (!apps || !current || apps.some((app) => !app.uuid)) return null;
    return { index: apps.findIndex((app) => app.uuid === current.uuid), count: apps.length };
  });

  async function move(uuid: string, delta: -1 | 1) {
    if (!apps) return;
    const from = apps.findIndex((app) => app.uuid === uuid);
    const to = from + delta;
    if (from < 0 || to < 0 || to >= apps.length) return;
    const next = apps.slice();
    const [moved] = next.splice(from, 1);
    if (moved) next.splice(to, 0, moved);
    await reorder(next);
  }

  async function saved(uuid: string, created: boolean) {
    await load();
    if (created) navigate(`/library/${encodeURIComponent(uuid)}`, { replace: true });
  }

  function deleted(uuid: string) {
    navigate('/library');
    if (apps) apps = apps.filter((app) => app.uuid !== uuid);
    void load();
  }
</script>

{#if editing === null}
  <PageHeader title="Library" subtitle="Games and programs that paired devices can start.">
    {#snippet actions()}
      <Button variant="primary" icon="plus" href="/library/new">Add app</Button>
    {/snippet}
  </PageHeader>

  {#if loadError}
    <div class="notice danger load-error" role="alert">
      <span>{apps ? 'Could not refresh the library' : 'Could not load the library'}: {loadError}</span>
      <Button size="sm" onclick={load}>Retry</Button>
    </div>
  {/if}

  <SteamSource onsynced={load} />
  <PlayniteSource onsynced={load} />

  {#if apps === null}
    {#if !loadError}<p class="muted placeholder">Loading apps…</p>{/if}
  {:else if apps.length === 0}
    <Panel>
      <EmptyState icon="library" title="No apps yet">
        <p>Add a game or program so devices can start it. An app without a command streams the desktop.</p>
        <Button variant="primary" icon="plus" href="/library/new">Add app</Button>
      </EmptyState>
    </Panel>
  {:else}
    <LibraryGrid {apps} {status} onreorder={reorder} onstatus={refreshStatus} />
  {/if}
{:else}
  {#key editing}
    {#if editing === 'new'}
      <AppEditor
        position={null}
        {status}
        onsaved={saved}
        onmove={move}
        ondeleted={deleted}
        onstatus={refreshStatus}
      />
    {:else if current}
      <AppEditor
        app={current}
        {position}
        {status}
        onsaved={saved}
        onmove={move}
        ondeleted={deleted}
        onstatus={refreshStatus}
      />
    {:else if apps === null}
      {#if loadError}
        <div class="notice danger load-error" role="alert">
          <span>Could not load the app: {loadError}</span>
          <Button size="sm" onclick={load}>Retry</Button>
        </div>
      {:else}
        <p class="muted placeholder">Loading the app…</p>
      {/if}
    {:else}
      <Panel>
        <EmptyState icon="library" title="App not found">
          <p>It may have been deleted.</p>
          <Button href="/library">Back to the library</Button>
        </EmptyState>
      </Panel>
    {/if}
  {/key}
{/if}

<style>
  .placeholder {
    font-size: var(--text-sm);
  }
  .load-error {
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
  }
</style>

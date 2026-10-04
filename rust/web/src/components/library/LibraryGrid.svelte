<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import AppCover from './AppCover.svelte';
  import type { App, SessionStatus } from '../../lib/api';
  import { link } from '../../lib/router.svelte';
  import { closeApp, confirmClose, coverPath, launchApp, runningName } from './app';

  let {
    apps,
    status,
    onreorder,
    onstatus,
  }: {
    apps: App[];
    status: SessionStatus | null;
    /** The whole list in its new order. */
    onreorder: (next: App[]) => void;
    /** An app was started or closed. */
    onstatus: () => void;
  } = $props();

  const uid = $props.id();
  let query = $state('');
  let sort = $state<'order' | 'name'>('order');
  /** `launch:uuid` or `close` while a request is out. */
  let busy = $state('');

  // While a card is dragged the grid shows this order; dropping saves it.
  let dragging = $state<string | null>(null);
  let preview = $state.raw<App[] | null>(null);

  const running = $derived(runningName(status));
  const needle = $derived(query.trim().toLowerCase());
  const missingIds = $derived(apps.some((app) => !app.uuid));
  const canReorder = $derived(sort === 'order' && !needle && !missingIds && apps.length > 1);

  const shown = $derived.by(() => {
    let list = preview ?? apps;
    if (needle) list = list.filter((app) => app.name.toLowerCase().includes(needle));
    if (sort === 'name') {
      list = [...list].sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }));
    }
    return list;
  });

  const hint = $derived.by(() => {
    if (apps.length < 2) return '';
    if (missingIds) return 'Some apps have no ID yet, so the order can’t be changed here.';
    if (canReorder) return 'Drag a cover to change the order.';
    return 'Clear the search and sort by library order to drag apps into place.';
  });

  async function launch(app: App) {
    busy = `launch:${app.uuid}`;
    await launchApp(app);
    busy = '';
    onstatus();
  }

  async function close() {
    const name = running;
    if (!(await confirmClose(name))) return;
    busy = 'close';
    await closeApp(name);
    busy = '';
    onstatus();
  }

  function sortable(node: HTMLElement) {
    const uuidAt = (event: DragEvent) =>
      event.target instanceof Element ? event.target.closest<HTMLElement>('[data-uuid]')?.dataset.uuid : undefined;
    const finish = () => {
      dragging = null;
      preview = null;
    };
    const start = (event: DragEvent) => {
      const uuid = uuidAt(event);
      if (!canReorder || !uuid) {
        event.preventDefault();
        return;
      }
      dragging = uuid;
      preview = apps.slice();
      if (event.dataTransfer) {
        event.dataTransfer.effectAllowed = 'move';
        // Firefox starts no drag without data.
        event.dataTransfer.setData('text/plain', uuid);
      }
    };
    const over = (event: DragEvent) => {
      if (!dragging || !preview) return;
      event.preventDefault();
      if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
      const target = uuidAt(event);
      if (!target || target === dragging) return;
      const from = preview.findIndex((app) => app.uuid === dragging);
      const to = preview.findIndex((app) => app.uuid === target);
      if (from < 0 || to < 0) return;
      // The dragged card takes the slot under the pointer.
      const next = preview.slice();
      const [moved] = next.splice(from, 1);
      if (moved) next.splice(to, 0, moved);
      preview = next;
    };
    const drop = (event: DragEvent) => {
      if (!dragging || !preview) return;
      event.preventDefault();
      const next = preview;
      finish();
      if (next.some((app, index) => app !== apps[index])) onreorder(next);
    };
    node.addEventListener('dragstart', start);
    node.addEventListener('dragover', over);
    node.addEventListener('drop', drop);
    node.addEventListener('dragend', finish);
    return () => {
      node.removeEventListener('dragstart', start);
      node.removeEventListener('dragover', over);
      node.removeEventListener('drop', drop);
      node.removeEventListener('dragend', finish);
    };
  }
</script>

<div class="library">
  <div class="toolbar">
    <div class="search">
      <label class="visually-hidden" for="{uid}-search">Search apps by name</label>
      <span class="search-icon"><Icon name="search" size={16} /></span>
      <input
        id="{uid}-search"
        class="input"
        type="search"
        placeholder="Search by name"
        autocomplete="off"
        spellcheck="false"
        bind:value={query}
      />
    </div>
    <div class="sort">
      <label for="{uid}-sort">Sort</label>
      <select id="{uid}-sort" class="select" bind:value={sort}>
        <option value="order">Library order</option>
        <option value="name">Name</option>
      </select>
    </div>
  </div>
  <p class="muted summary">
    <span>{apps.length} {apps.length === 1 ? 'app' : 'apps'}</span>
    {#if hint}<span aria-hidden="true">·</span> <span>{hint}</span>{/if}
  </p>

  {#if shown.length === 0}
    <p class="muted none">No apps match “{query.trim()}”.</p>
  {:else}
    <ul class="grid" aria-label="Apps" {@attach sortable}>
      {#each shown as app (app)}
        {@const isRunning = running !== '' && app.name === running}
        <li
          class="card"
          class:dragging={dragging !== null && app.uuid === dragging}
          data-uuid={app.uuid}
          draggable={canReorder && !!app.uuid}
        >
          {#if app.uuid}
            <a class="open" href="/library/{encodeURIComponent(app.uuid)}" use:link draggable="false">
              <div class="art">
                <AppCover name={app.name} uuid={app.uuid} path={coverPath(app)} />
                {#if isRunning}
                  <span class="state">
                    {#if status?.paused}<Badge tone="warn">Paused</Badge>{:else}<Badge tone="live">Running</Badge>{/if}
                  </span>
                {/if}
              </div>
              <span class="name">{app.name || 'Unnamed app'}</span>
            </a>
          {:else}
            <div class="open" title="This app has no ID yet. Restart the host to edit it here.">
              <div class="art"><AppCover name={app.name} /></div>
              <span class="name">{app.name || 'Unnamed app'}</span>
              <span class="muted note">Read-only, no ID yet</span>
            </div>
          {/if}
          <div class="actions">
            {#if isRunning}
              <Button size="sm" icon="stop" busy={busy === 'close'} onclick={close}>
                Close<span class="visually-hidden"> {app.name}</span>
              </Button>
            {:else}
              <Button
                size="sm"
                icon="play"
                disabled={!app.uuid || running !== '' || busy !== ''}
                busy={busy === `launch:${app.uuid}`}
                title={running ? `Close ${running} first` : undefined}
                onclick={() => launch(app)}
              >
                Launch<span class="visually-hidden"> {app.name}</span>
              </Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .toolbar {
    display: flex;
    gap: var(--space-2) var(--space-3);
    flex-wrap: wrap;
    align-items: center;
  }
  .search {
    position: relative;
    flex: 1 1 220px;
    max-width: 360px;
  }
  .search-icon {
    position: absolute;
    left: 10px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--muted);
    pointer-events: none;
  }
  .search .input {
    padding-left: 32px;
  }
  .sort {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .sort label {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .sort .select {
    width: auto;
  }
  .library {
    display: grid;
    gap: var(--space-3);
  }
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 0 6px;
    font-size: var(--text-xs);
  }
  .none {
    font-size: var(--text-sm);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(136px, 1fr));
    gap: var(--space-5) var(--space-4);
    margin: var(--space-2) 0 0;
    padding: 0;
    list-style: none;
  }
  .card {
    display: grid;
    gap: var(--space-2);
    align-content: start;
    min-width: 0;
  }
  .card[draggable='true'] {
    cursor: grab;
  }
  .dragging {
    opacity: 0.4;
  }
  .open {
    display: grid;
    gap: 6px;
    min-width: 0;
    color: inherit;
    text-decoration: none;
    border-radius: var(--radius);
  }
  .art {
    position: relative;
    border-radius: var(--radius);
  }
  a.open:hover .art {
    box-shadow: 0 0 0 1px var(--muted);
  }
  a.open:hover .name {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .state {
    position: absolute;
    top: 6px;
    left: 6px;
  }
  .name {
    font-weight: 560;
    font-size: var(--text-sm);
    line-height: 1.35;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
  .note {
    font-size: var(--text-xs);
  }
  .actions {
    display: flex;
  }
</style>

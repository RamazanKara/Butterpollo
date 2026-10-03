<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import Panel from '../Panel.svelte';
  import LoadState from './LoadState.svelte';
  import { api, type UpdatesState } from '../../lib/api';
  import { failed } from '../../lib/feedback.svelte';
  import { ago } from '../../lib/format';
  import { session } from '../../lib/session.svelte';
  import { localTime, reason, sleep } from './util';

  let updates = $state<UpdatesState | null>(null);
  let loadError = $state('');
  let checking = $state(false);
  let alive = true;
  onDestroy(() => (alive = false));

  const installed = $derived(session.metadata?.version ?? '');
  const latest = $derived(updates?.releases.find((release) => !release.prerelease) ?? null);
  // GitHub sends null for a release without notes.
  const notes = $derived(latest?.body?.trim() ?? '');
  const available = $derived(latest !== null && installed !== '' && newer(latest.tag_name, installed));

  function numbers(version: string): number[] | null {
    const match = /\d+(?:\.\d+)*/.exec(version);
    return match ? match[0].split('.').map(Number) : null;
  }

  /** Loose comparison: tags may start with "v"; a build newer than the release is not an update. */
  function newer(tag: string, version: string): boolean {
    const clean = (value: string) => value.trim().replace(/^v/i, '').toLowerCase();
    if (clean(tag) === clean(version)) return false;
    const a = numbers(tag);
    const b = numbers(version);
    if (!a || !b) return true;
    for (let index = 0; index < Math.max(a.length, b.length); index++) {
      const x = a[index] ?? 0;
      const y = b[index] ?? 0;
      if (x !== y) return x > y;
    }
    // Same numbers: a pre-release build (2.0.0-beta) is older than the release.
    return clean(version).includes('-') && !clean(tag).includes('-');
  }

  async function load() {
    try {
      updates = await api.updates.state();
      loadError = '';
      if (updates.checking) await settle();
    } catch (error) {
      loadError = reason(error);
    }
  }

  /** Poll while the host is asking GitHub. */
  async function settle() {
    checking = true;
    try {
      const deadline = Date.now() + 60_000;
      do {
        await sleep(1000);
        updates = await api.updates.state();
      } while (alive && updates.checking && Date.now() < deadline);
    } finally {
      checking = false;
    }
  }

  async function check() {
    try {
      await api.updates.check();
      await settle();
    } catch (error) {
      failed('Checking for updates failed', error);
    }
  }

  onMount(load);
</script>

<Panel title="Updates">
  {#snippet actions()}
    {#if available}
      <Badge tone="accent">Update available</Badge>
    {:else if latest}
      <Badge tone="ok">Up to date</Badge>
    {/if}
  {/snippet}
  {#if updates}
    <div class="stack">
      <dl class="facts">
        <dt>Installed</dt>
        <dd class="num">{installed || '…'}</dd>
        <dt>Latest release</dt>
        <dd>
          {#if latest}
            <span class="num">{latest.tag_name}</span>
            {#if latest.published_at}<span class="muted"> · {localTime(latest.published_at)}</span>{/if}
          {:else if updates.checked_at}
            <span class="muted">No releases found</span>
          {:else}
            <span class="muted">Not checked yet</span>
          {/if}
        </dd>
        <dt>Last checked</dt>
        <dd>{updates.checked_at ? ago(updates.checked_at) : 'Never'}</dd>
      </dl>
      {#if updates.check_failed && !checking}
        <p class="notice warn" role="alert">The last check failed. Check that this PC can reach github.com.</p>
      {/if}
      {#if latest}
        {#if notes}
          <details>
            <summary>Release notes for {latest.name || latest.tag_name}</summary>
            <pre class="notes">{notes}</pre>
          </details>
        {/if}
        <a class="release" href={latest.html_url} target="_blank" rel="noreferrer">
          Open the release page<Icon name="external" size={14} />
        </a>
      {/if}
      <div class="row">
        <Button icon="refresh" busy={checking} onclick={check}>Check now</Button>
      </div>
    </div>
  {:else}
    <LoadState error={loadError} what="the update status" />
  {/if}
</Panel>

<style>
  details {
    border: 1px solid var(--line);
    border-radius: var(--radius);
    font-size: var(--text-sm);
  }
  summary {
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    font-weight: 560;
  }
  .notes {
    margin: 0;
    padding: var(--space-3);
    border-top: 1px solid var(--line);
    background: var(--sunken);
    max-height: 320px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
    line-height: 1.55;
  }
  .release {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-sm);
    justify-self: start;
  }
</style>

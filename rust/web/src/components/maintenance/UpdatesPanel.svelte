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
  import { localTime, reason } from './util';

  let updates = $state<UpdatesState | null>(null);
  let loadError = $state('');
  let acting = $state(false);
  let reconnecting = $state(false);
  let timer: ReturnType<typeof setTimeout>;
  let alive = true;
  onDestroy(() => { alive = false; clearTimeout(timer); });
  const installed = $derived(session.metadata?.version ?? '');
  const latest = $derived(updates?.releases.find((release) => release.tag_name === updates?.latest_version) ?? updates?.releases[0] ?? null);
  const notes = $derived(latest?.body?.trim() ?? '');
  const available = $derived(updates?.update_available ?? false);
  const queued = $derived(['waiting', 'downloading', 'ready', 'installing'].includes(updates?.phase ?? ''));
  const progress = $derived(Math.min(100, Math.floor(100 * (updates?.downloaded_bytes ?? 0) / (updates?.download_size || 1))));

  async function load() {
    clearTimeout(timer);
    try {
      const previous = updates?.phase;
      updates = await api.updates.state();
      loadError = '';
      reconnecting = false;
      if (previous === 'installing' && updates.phase !== 'installing') session.metadata = await api.metadata();
    } catch (error) {
      if (updates?.phase === 'installing') reconnecting = true;
      else loadError = reason(error);
    } finally {
      if (alive) {
        const active = updates?.checking || queued || updates?.last_install?.phase === 'installing';
        timer = setTimeout(load, active ? 2000 : 30000);
      }
    }
  }
  async function act(action: 'check' | 'install' | 'cancel') {
    acting = true;
    try { await api.updates[action](); await load(); }
    catch (error) { failed('Update request failed', error); }
    finally { acting = false; }
  }
  onMount(load);
</script>

<Panel title="Updates">
  {#snippet actions()}
    {#if available}<Badge tone="accent">Update available</Badge>
    {:else if latest && !updates?.check_failed}<Badge tone="ok">Up to date</Badge>{/if}
  {/snippet}
  {#if updates}
    <div class="stack">
      <dl class="facts">
        <dt>Installed</dt><dd class="num">{installed || '…'}</dd>
        <dt>Latest release</dt>
        <dd>
          {#if latest}<span class="num">{latest.tag_name}</span>
            {#if latest.prerelease}<Badge>Pre-release</Badge>{/if}
            {#if latest.published_at}<span class="muted"> · {localTime(latest.published_at)}</span>{/if}
          {:else}<span class="muted">{updates.checked_at ? 'No releases found for your update settings' : 'Not checked yet'}</span>{/if}
        </dd>
        <dt>Last checked</dt><dd>{updates.checked_at ? ago(updates.checked_at) : 'Never'}</dd>
        <dt>Automatic installation</dt><dd>{updates.auto_update ? 'On — installs when idle' : 'Off — notify me first'}</dd>
      </dl>
      <p class="muted">Updates wait for streams, pending connections and host apps to stop, then install after one minute of idle time. Settings and paired devices stay in place.</p>
      <a href="/settings/general">Change update settings</a>
      {#if !updates.install_supported}<p class="notice">In-app installation requires the installed Windows service. Use the release page for portable builds.</p>{/if}
      {#if updates.check_failed}<p class="notice warn" role="alert">The last check failed. {updates.check_error || 'Check that this PC can reach GitHub.'}</p>{/if}
      {#if loadError}<p class="notice warn" role="alert">Could not refresh update status: {loadError}</p>{/if}
      {#if updates.phase === 'waiting' || updates.phase === 'ready'}
        <p class="notice" role="status">{updates.queued_version} is queued. Waiting for the host to be idle. Disconnect clients and quit the host app to continue.</p>
      {:else if updates.phase === 'downloading'}
        <p class="notice" role="status">Downloading {updates.queued_version}: {progress}%</p>
        <progress max="100" value={progress} aria-label="Update download">{progress}%</progress>
      {:else if updates.phase === 'installing'}
        <p class="notice" role="status">{reconnecting ? 'Butterpollo is restarting. Reconnecting…' : 'Installing the update. Butterpollo will restart shortly…'}</p>
      {:else if updates.phase === 'failed'}
        <p class="notice warn" role="alert">{updates.error}</p>
      {/if}
      {#if updates.last_install && !queued}
        <p class:warn={updates.last_install.phase !== 'installed'} class="notice" role="status">
          {#if updates.last_install.phase === 'installed'}Installed {updates.last_install.version} successfully.
          {:else if updates.last_install.error}{updates.last_install.error}
          {:else}The previous update did not report completion. Check the setup log before retrying.{/if}
        </p>
      {/if}
      {#if latest}
        {#if notes}<details><summary>Release notes for {latest.name || latest.tag_name}</summary><pre class="notes">{notes}</pre></details>{/if}
        <a class="release" href={latest.html_url} target="_blank" rel="noreferrer">Open the release page<Icon name="external" size={14} /></a>
      {/if}
      <div class="row">
        <Button icon="refresh" busy={acting || updates.checking} disabled={queued} onclick={() => act('check')}>Check now</Button>
        {#if queued && updates.phase !== 'installing'}
          <Button disabled={acting} onclick={() => act('cancel')}>Cancel update</Button>
        {:else if available && updates.install_supported && !queued}
          <Button disabled={acting} onclick={() => act('install')}>Install when idle</Button>
        {/if}
      </div>
    </div>
  {:else}<LoadState error={loadError} what="the update status" />{/if}
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

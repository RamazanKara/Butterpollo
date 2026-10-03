<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../components/Badge.svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import Panel from '../components/Panel.svelte';
  import ConnectHelp from '../components/overview/ConnectHelp.svelte';
  import ConnectionList from '../components/overview/ConnectionList.svelte';
  import HostReadiness from '../components/overview/HostReadiness.svelte';
  import RunningApp from '../components/overview/RunningApp.svelte';
  import StreamCard from '../components/overview/StreamCard.svelte';
  import { api, type SessionStatus, type StreamSession } from '../lib/api';
  import { confirm, failed, notify } from '../lib/feedback.svelte';
  import { poll } from '../lib/format';
  import { link } from '../lib/router.svelte';
  import { refreshMetadata, session } from '../lib/session.svelte';

  let sessions = $state<StreamSession[] | null>(null);
  let status = $state<SessionStatus | null>(null);
  let sessionsError = $state<string | null>(null);
  let statusError = $state<string | null>(null);
  let metadataError = $state<string | null>(null);
  let rechecking = $state(false);
  /** uuid of the device being disconnected. */
  let disconnecting = $state<string | null>(null);

  const metadata = $derived(session.metadata);
  const streams = $derived((sessions ?? []).filter((item) => item.role === 'stream'));
  const others = $derived((sessions ?? []).filter((item) => item.role !== 'stream'));
  const live = $derived(streams.filter((item) => item.state === 'RUNNING').length);
  const addresses = $derived(
    metadata ? (metadata.pc_addresses ?? []).filter((address) => address !== metadata.pc_address) : [],
  );

  const devices = (count: number) => `${count} ${count === 1 ? 'device' : 'devices'}`;
  const reason = (failure: unknown) => (failure instanceof Error ? failure.message : String(failure));

  const summary = $derived.by(() => {
    if (!sessions) return sessionsError ? "Can't reach the host." : 'Checking for streams…';
    const app = status?.appRunning ? status.app?.name || status.appName : '';
    if (live) return app ? `Streaming ${app} to ${devices(live)}.` : `Streaming to ${devices(live)}.`;
    if (app) return `Nothing is streaming. ${app} is ${status?.paused ? 'paused' : 'running'}.`;
    return 'Nothing is streaming.';
  });

  async function refreshSessions() {
    const [list, current] = await Promise.allSettled([api.sessions.streams(), api.sessions.status()]);
    if (list.status === 'fulfilled') {
      sessions = list.value.sessions ?? [];
      sessionsError = null;
    } else {
      sessionsError = reason(list.reason);
    }
    if (current.status === 'fulfilled') {
      status = current.value;
      statusError = null;
    } else {
      statusError = reason(current.reason);
    }
  }

  async function loadMetadata() {
    try {
      await refreshMetadata();
      metadataError = null;
    } catch (failure) {
      metadataError = reason(failure);
    }
  }

  async function recheck() {
    rechecking = true;
    await loadMetadata();
    rechecking = false;
  }

  async function disconnect(target: StreamSession) {
    const name = target.device_name || 'this device';
    const ok = await confirm({
      title: `Disconnect ${name}?`,
      message: 'Every connection from this device ends. The running app stays open.',
      confirm: 'Disconnect',
      danger: true,
    });
    if (!ok) return;
    disconnecting = target.uuid;
    try {
      await api.clients.disconnect(target.uuid);
      notify(`Disconnected ${name}.`, 'ok');
      await refreshSessions();
    } catch (failure) {
      failed(`Disconnecting ${name} failed`, failure);
    } finally {
      disconnecting = null;
    }
  }

  onMount(() => {
    const stopSessions = poll(refreshSessions, 2000);
    const stopMetadata = poll(loadMetadata, 30_000);
    return () => {
      stopSessions();
      stopMetadata();
    };
  });
</script>

<PageHeader title="Overview" subtitle={summary} />

<div class="frame">
  <div class="columns">
    <div class="stack">
      <Panel title="Streaming now" flush>
        {#snippet actions()}
          {#if live}<Badge tone="live">{live} live</Badge>{/if}
        {/snippet}
        {#if sessionsError}
          <p class="notice danger lost" role="alert">
            {sessions ? `Lost contact with the host: ${sessionsError}. Showing the last known state.` : `Can't load streams: ${sessionsError}`}
          </p>
        {/if}
        {#if !sessions}
          {#if !sessionsError}<p class="placeholder muted">Checking for streams…</p>{/if}
        {:else}
          {#if streams.length}
            <div class="streams">
              {#each streams as stream}
                <StreamCard {stream} busy={disconnecting === stream.uuid} ondisconnect={disconnect} />
              {/each}
            </div>
          {:else}
            <ConnectHelp {metadata} />
          {/if}
          {#if others.length}
            <ConnectionList sessions={others} busy={disconnecting} ondisconnect={disconnect} />
          {/if}
        {/if}
      </Panel>

      <RunningApp {status} error={statusError} onchange={refreshSessions} />
    </div>

    <div class="stack">
      <HostReadiness {metadata} error={metadataError} checking={rechecking} onrecheck={recheck} />

      <Panel title="Host">
        {#if metadata}
          <dl class="facts">
            <dt>Name</dt>
            <dd>{metadata.host_name}</dd>
            <dt>Address</dt>
            <dd class="num">{metadata.pc_address}</dd>
            {#if addresses.length}
              <dt>Other addresses</dt>
              <dd class="num">{addresses.join(', ')}</dd>
            {/if}
            <dt>Version</dt>
            <dd class="num">{metadata.version}</dd>
            <dt>Paired devices</dt>
            <dd><a class="num" href="/devices" use:link>{metadata.paired_devices}</a></dd>
          </dl>
        {:else}
          <p class="muted">{metadataError ? 'Host details are unavailable.' : 'Loading host details…'}</p>
        {/if}
      </Panel>
    </div>
  </div>
</div>

<style>
  .frame {
    container: overview / inline-size;
  }
  .columns {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  @container overview (min-width: 760px) {
    .columns {
      grid-template-columns: minmax(0, 1fr) minmax(280px, 340px);
    }
  }
  .streams > :global(article + article) {
    border-top: 1px solid var(--line);
  }
  .placeholder {
    padding: var(--space-4) var(--space-5);
    font-size: var(--text-sm);
  }
  .lost {
    margin: var(--space-3) var(--space-5);
  }
</style>

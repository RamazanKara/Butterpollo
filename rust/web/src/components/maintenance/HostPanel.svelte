<script lang="ts">
  import { onDestroy } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import { api } from '../../lib/api';
  import { confirm, failed, notify } from '../../lib/feedback.svelte';
  import { refreshMetadata, session } from '../../lib/session.svelte';
  import { sleep } from './util';

  let phase = $state<'running' | 'restarting' | 'late' | 'stopped'>('running');
  let alive = true;
  onDestroy(() => (alive = false));

  async function restart() {
    const ok = await confirm({
      title: 'Restart Rubylight?',
      message: 'Streams in progress disconnect. This console reconnects by itself when the host is back.',
      confirm: 'Restart',
    });
    if (!ok) return;
    try {
      await api.host.restart();
    } catch (error) {
      failed('Restart failed', error);
      return;
    }
    phase = 'restarting';
    // The host answers for a moment before it stops; wait until it has been away and is back.
    const started = Date.now();
    let away = false;
    while (alive && Date.now() - started < 90_000) {
      await sleep(1000);
      try {
        await refreshMetadata();
        if (away || Date.now() - started > 10_000) {
          phase = 'running';
          notify('Rubylight restarted.', 'ok');
          return;
        }
      } catch {
        away = true;
      }
    }
    if (alive) phase = 'late';
  }

  async function quit() {
    const ok = await confirm({
      title: 'Quit Rubylight?',
      message:
        'Streaming stops and this console goes offline.\n\nWhen Rubylight runs as a service, Windows restarts it after a crash but not after Quit. It stays stopped until you start the service again or restart the PC.',
      confirm: 'Quit',
      danger: true,
    });
    if (!ok) return;
    try {
      await api.host.quit();
      phase = 'stopped';
    } catch (error) {
      failed('Quit failed', error);
    }
  }
</script>

<Panel title="Host">
  {#snippet actions()}
    {#if phase === 'stopped'}
      <Badge tone="danger">Stopped</Badge>
    {:else if phase === 'restarting'}
      <Badge tone="warn">Restarting</Badge>
    {:else if phase === 'late'}
      <Badge tone="warn">Not responding</Badge>
    {:else}
      <Badge tone="ok">Running</Badge>
    {/if}
  {/snippet}
  <div class="stack">
    <dl class="facts">
      <dt>Version</dt>
      <dd class="num">{session.metadata?.version ?? '…'}</dd>
      <dt>Host name</dt>
      <dd>{session.metadata?.host_name ?? '…'}</dd>
    </dl>
    {#if phase === 'restarting'}
      <p class="notice" role="status">Restarting. This console reconnects when the host is back.</p>
    {:else if phase === 'late'}
      <p class="notice warn" role="alert">The host has not come back after 90 seconds. Check the PC, then reload this page.</p>
    {:else if phase === 'stopped'}
      <p class="notice danger" role="status">Rubylight has stopped. This console is offline until the host starts again.</p>
    {/if}
    <div class="row">
      <Button icon="refresh" busy={phase === 'restarting'} disabled={phase === 'stopped'} onclick={restart}>Restart</Button>
      <Button variant="danger" icon="power" disabled={phase === 'stopped' || phase === 'restarting'} onclick={quit}>Quit</Button>
    </div>
    <p class="muted note">
      Restart disconnects streams and applies settings that need it. Quit stops the host; if it runs as a service, it
      stays stopped until the service is started again or the PC restarts.
    </p>
  </div>
</Panel>

<style>
  .note {
    font-size: var(--text-sm);
  }
</style>

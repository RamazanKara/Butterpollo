<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import { api, type SessionStatus } from '../../lib/api';
  import { confirm, failed, notify } from '../../lib/feedback.svelte';

  let {
    status,
    error,
    onchange,
  }: { status: SessionStatus | null; error: string | null; onchange: () => void } = $props();

  let closing = $state(false);
  const name = $derived(status?.app?.name || status?.appName || 'Unnamed app');

  async function close() {
    const app = name;
    const ok = await confirm({
      title: `Close ${app}?`,
      message: 'The app quits on this PC and every stream ends. Unsaved progress in the app may be lost.',
      confirm: 'Close app',
      danger: true,
    });
    if (!ok) return;
    closing = true;
    try {
      await api.apps.close();
      notify(`Closed ${app}.`, 'ok');
      onchange();
    } catch (failure) {
      failed('Closing the app failed', failure);
    } finally {
      closing = false;
    }
  }
</script>

<Panel title="Running app">
  {#if !status}
    <p class="muted">{error ? `Can't check for a running app: ${error}` : 'Checking for a running app…'}</p>
  {:else if status.appRunning}
    <div class="app">
      <div class="text">
        <div class="name">
          <strong>{name}</strong>
          {#if status.paused}
            <Badge tone="warn">Paused</Badge>
          {:else}
            <Badge tone="live">Running</Badge>
          {/if}
        </div>
        <p class="muted">
          {status.paused
            ? 'No device is connected. It keeps running until a device resumes it or you close it.'
            : 'Closing it ends every stream.'}
        </p>
      </div>
      <Button variant="danger" size="sm" icon="stop" busy={closing} onclick={close}>Close app</Button>
    </div>
  {:else}
    <p class="muted">No app is running. A device starts one when it connects.</p>
  {/if}
</Panel>

<style>
  .app {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3) var(--space-4);
    flex-wrap: wrap;
  }
  .text {
    display: grid;
    gap: 2px;
    min-width: 0;
    flex: 1 1 240px;
  }
  .name {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  strong {
    overflow-wrap: anywhere;
  }
  p {
    font-size: var(--text-sm);
  }
</style>

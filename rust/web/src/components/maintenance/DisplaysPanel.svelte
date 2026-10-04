<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import LoadState from './LoadState.svelte';
  import { api, type GoldenStatus } from '../../lib/api';
  import { confirm, failed, notify } from '../../lib/feedback.svelte';
  import { reason } from './util';

  type Action = 'save' | 'restore' | 'delete' | 'disconnect' | 'reset';

  let golden = $state<GoldenStatus | null>(null);
  let loadError = $state('');
  let running = $state<Action | null>(null);

  async function load() {
    try {
      golden = await api.displays.goldenStatus();
      loadError = '';
    } catch (error) {
      loadError = reason(error);
    }
  }

  async function run(action: Action, task: () => Promise<unknown>, done: string, failure: string) {
    running = action;
    try {
      await task();
      notify(done, 'ok');
    } catch (error) {
      failed(failure, error);
    } finally {
      running = null;
    }
    await load();
  }

  async function save() {
    if (
      golden?.exists &&
      !(await confirm({
        title: 'Replace the saved layout?',
        message: 'The current display layout replaces the one saved before.',
        confirm: 'Replace',
      }))
    )
      return;
    await run('save', api.displays.exportGolden, 'Display layout saved.', 'Saving the layout failed');
  }

  const restore = () =>
    run('restore', api.displays.restoreGolden, 'Saved layout restored.', 'Restoring the layout failed');
  const reset = () =>
    run('reset', api.displays.resetPersistence, 'Display settings memory reset.', 'Resetting display settings failed');

  async function remove() {
    const ok = await confirm({
      title: 'Delete the saved layout?',
      message: 'Butterpollo stops restoring a layout after streams until you save one again.',
      confirm: 'Delete',
      danger: true,
    });
    if (ok) await run('delete', api.displays.deleteGolden, 'Saved layout deleted.', 'Deleting the layout failed');
  }

  async function disconnect() {
    const ok = await confirm({
      title: 'Disconnect virtual displays?',
      message: 'This stops every stream and removes the virtual displays Butterpollo created.',
      confirm: 'Disconnect',
      danger: true,
    });
    if (ok)
      await run('disconnect', api.displays.disconnectVirtual, 'Virtual displays disconnected.', 'Disconnecting failed');
  }

  onMount(load);
</script>

<Panel title="Displays" description="Butterpollo restores the saved layout after each stream, if all of its displays are connected.">
  {#snippet actions()}
    {#if golden?.exists}
      {#if golden.comparison_available && golden.current_mismatch_reason}
        <Badge tone="warn">Differs from current</Badge>
      {:else if golden.comparison_available}
        <Badge tone="ok">Matches current</Badge>
      {:else}
        <Badge>Saved</Badge>
      {/if}
    {:else if golden}
      <Badge>No saved layout</Badge>
    {/if}
  {/snippet}
  {#if golden}
    <div class="stack">
      <div class="group">
        <p class="text">
          {#if !golden.exists}
            No layout is saved. Arrange your displays the way you want them between streams, then save.
          {:else if golden.current_mismatch_reason}
            A layout is saved. The displays are arranged differently right now; restore it to go back.
          {:else}
            A layout is saved and matches the displays as they are now.
          {/if}
        </p>
        <div class="row">
          <Button disabled={running !== null} busy={running === 'save'} onclick={save}>Save current layout</Button>
          <Button disabled={!golden.exists || running !== null} busy={running === 'restore'} onclick={restore}>
            Restore saved layout
          </Button>
          <Button variant="danger" disabled={!golden.exists || running !== null} busy={running === 'delete'} onclick={remove}>
            Delete saved layout
          </Button>
        </div>
      </div>
      <div class="group recovery">
        <h3>Recovery</h3>
        <div class="action">
          <div>
            <p class="text">Disconnect virtual displays</p>
            <p class="muted hint">Stops every stream and removes the virtual displays Butterpollo created.</p>
          </div>
          <Button variant="danger" disabled={running !== null} busy={running === 'disconnect'} onclick={disconnect}>
            Disconnect
          </Button>
        </div>
        <div class="action">
          <div>
            <p class="text">Reset display settings memory</p>
            <p class="muted hint">
              Forgets the display changes Butterpollo would undo after a stream. Use it when displays keep returning to an
              old setup.
            </p>
          </div>
          <Button disabled={running !== null} busy={running === 'reset'} onclick={reset}>Reset</Button>
        </div>
        <p class="muted hint">Saving, restoring and resetting fail while a stream is running.</p>
      </div>
    </div>
  {:else}
    <LoadState error={loadError} what="the display layout" />
  {/if}
</Panel>

<style>
  .group {
    display: grid;
    gap: var(--space-3);
  }
  .recovery {
    padding-top: var(--space-4);
    border-top: 1px solid var(--line);
  }
  h3 {
    font-size: var(--text-sm);
  }
  .text {
    font-size: var(--text-sm);
  }
  .hint {
    font-size: var(--text-xs);
    line-height: 1.45;
  }
  .action {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .action > div {
    flex: 1 1 220px;
    display: grid;
    gap: 2px;
  }
</style>

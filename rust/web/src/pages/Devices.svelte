<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../components/Badge.svelte';
  import Button from '../components/Button.svelte';
  import EmptyState from '../components/EmptyState.svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import Panel from '../components/Panel.svelte';
  import DeviceEditor from '../components/devices/DeviceEditor.svelte';
  import PairPanel from '../components/devices/PairPanel.svelte';
  import { grantedLabels, permissionSummary } from '../components/devices/device';
  import { api, type Client } from '../lib/api';
  import { confirm, failed, notify } from '../lib/feedback.svelte';
  import { poll } from '../lib/format';

  let clients = $state<Client[] | null>(null);
  let loadError = $state('');
  let editing = $state<string | null>(null);
  let editorDirty = $state(false);
  /** `action:uuid` of the request in flight. */
  let busy = $state('');
  let latestLoad = 0;

  async function load() {
    // A poll that started before a save must not put the old values back.
    const id = ++latestLoad;
    try {
      const list = (await api.clients.list()).clients;
      if (id !== latestLoad) return;
      clients = list;
      loadError = '';
      if (editing && !list.some((client) => client.uuid === editing)) closeEditor();
    } catch (error) {
      if (id === latestLoad) loadError = error instanceof Error ? error.message : String(error);
    }
  }

  onMount(() => poll(load, 5000));

  function closeEditor() {
    editing = null;
    editorDirty = false;
  }

  async function toggleEditor(uuid: string) {
    if (
      editorDirty &&
      !(await confirm({
        title: 'Discard changes?',
        message: 'The changes you made to this device are not saved.',
        confirm: 'Discard',
      }))
    )
      return;
    const open = editing !== uuid;
    closeEditor();
    if (open) editing = uuid;
  }

  async function run(key: string, action: () => Promise<unknown>, success: string, failure: string) {
    busy = key;
    try {
      await action();
      notify(success, 'ok');
    } catch (error) {
      failed(failure, error);
    } finally {
      busy = '';
    }
    await load();
  }

  async function disconnect(client: Client) {
    const ok = await confirm({
      title: `Disconnect ${client.name}?`,
      message: 'Its stream ends now. The device stays paired and can connect again.',
      confirm: 'Disconnect',
    });
    if (ok) {
      await run(
        `disconnect:${client.uuid}`,
        () => api.clients.disconnect(client.uuid),
        `Disconnected ${client.name}.`,
        'Disconnecting failed',
      );
    }
  }

  async function unpair(client: Client) {
    const ok = await confirm({
      title: `Unpair ${client.name}?`,
      message: 'It is disconnected and removed. The device must pair again before it can stream.',
      confirm: 'Unpair',
      danger: true,
    });
    if (ok) {
      await run(`unpair:${client.uuid}`, () => api.clients.unpair(client.uuid), `Unpaired ${client.name}.`, 'Unpairing failed');
    }
  }

  async function unpairAll() {
    const count = clients?.length ?? 0;
    const ok = await confirm({
      title: 'Unpair all devices?',
      message: `${count === 1 ? 'The paired device is' : `All ${count} paired devices are`} disconnected and removed. Each must pair again before it can stream.`,
      confirm: 'Unpair all',
      danger: true,
    });
    if (ok) {
      closeEditor();
      await run('unpair-all', () => api.clients.unpairAll(), 'Unpaired all devices.', 'Unpairing failed');
    }
  }
</script>

<PageHeader title="Devices" subtitle="Pair Moonlight and Artemis devices and choose what each one can do." />

<PairPanel />

<Panel
  title="Paired devices"
  description={clients ? `${clients.length} ${clients.length === 1 ? 'device' : 'devices'}` : undefined}
  flush
>
  {#snippet actions()}
    <Button variant="danger" size="sm" disabled={!clients?.length} busy={busy === 'unpair-all'} onclick={unpairAll}>
      Unpair all
    </Button>
  {/snippet}

  {#if loadError}
    <div class="load-error notice danger" role="alert">
      <span>{clients ? 'Could not refresh the list' : 'Could not load devices'}: {loadError}</span>
      <Button size="sm" onclick={load}>Retry</Button>
    </div>
  {/if}

  {#if clients === null}
    {#if !loadError}<p class="placeholder muted">Loading devices…</p>{/if}
  {:else if clients.length === 0}
    <EmptyState icon="devices" title="No paired devices">
      <p>Pair a device above. It shows up here once pairing finishes.</p>
    </EmptyState>
  {:else}
    <ul class="devices">
      {#each clients as client (client.uuid)}
        {@const open = editing === client.uuid}
        <li class:open>
          <div class="device">
            <div class="identity">
              <div class="title">
                <span class="name">{client.name}</span>
                {#if client.connected}<Badge tone="live">Connected</Badge>{/if}
                {#if !client.enabled}<Badge>Disabled</Badge>{/if}
              </div>
              <div class="meta muted">
                <span title={grantedLabels(client.perm) || 'No permissions'}>{permissionSummary(client.perm)}</span>
              </div>
            </div>
            <div class="actions">
              {#if client.connected}
                <Button size="sm" busy={busy === `disconnect:${client.uuid}`} onclick={() => disconnect(client)}>
                  Disconnect<span class="visually-hidden"> {client.name}</span>
                </Button>
              {/if}
              <Button size="sm" icon={open ? 'x' : 'edit'} onclick={() => toggleEditor(client.uuid)}>
                {open ? 'Close' : 'Edit'}<span class="visually-hidden"> {client.name}</span>
              </Button>
              <Button
                size="sm"
                variant="danger"
                busy={busy === `unpair:${client.uuid}`}
                onclick={() => unpair(client)}
              >
                Unpair<span class="visually-hidden"> {client.name}</span>
              </Button>
            </div>
          </div>
          {#if open}
            <DeviceEditor {client} bind:dirty={editorDirty} onsaved={load} />
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  .placeholder {
    padding: var(--space-5);
    font-size: var(--text-sm);
  }
  .load-error {
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    margin: var(--space-3) var(--space-5);
  }
  .devices {
    margin: 0;
    padding: 0;
    list-style: none;
    /* clip, not hidden, so the editor's sticky footer still sticks. */
    overflow: clip;
    border-radius: 0 0 calc(var(--radius-lg) - 1px) calc(var(--radius-lg) - 1px);
  }
  .devices > li + li {
    border-top: 1px solid var(--line);
  }
  .device {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2) var(--space-4);
    flex-wrap: wrap;
    padding: var(--space-3) var(--space-5);
  }
  .open .device {
    background: var(--sunken);
  }
  .identity {
    display: grid;
    gap: 2px;
    flex: 1 1 220px;
    min-width: 0;
  }
  .title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .name {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  @media (max-width: 520px) {
    .device {
      padding: var(--space-3) var(--space-4);
    }
    .load-error {
      margin: var(--space-3) var(--space-4);
    }
  }
</style>

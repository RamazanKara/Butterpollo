<script lang="ts">
  // The Steam line above the library: what the sync covers, and Sync now.
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import { api, type SteamStatus } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';

  let { onsynced }: { onsynced?: () => void } = $props();

  let status = $state<SteamStatus | null>(null);
  let syncing = $state(false);

  async function load() {
    try {
      status = await api.steam.status();
    } catch {
      // An older host without Steam support: show nothing.
      status = null;
    }
  }

  onMount(() => {
    void load();
  });

  const scope = $derived.by(() => {
    if (!status) return '';
    const games = status.selected_game_count === 1 ? 'game' : 'games';
    const which = status.sync_all_installed ? 'installed' : 'recently played';
    return `${status.selected_game_count} ${which} ${games} of ${status.game_count}`;
  });

  async function sync() {
    syncing = true;
    try {
      const result = await api.steam.sync();
      notify(result.changed ? 'Steam games updated.' : 'Steam games are up to date.', 'ok');
      onsynced?.();
      await load();
    } catch (error) {
      failed('Syncing Steam failed', error);
    } finally {
      syncing = false;
    }
  }
</script>

{#if status?.enabled}
  <div class="source">
    <Icon name="gamepad" />
    <div class="text">
      <strong>Steam</strong>
      <span class="muted">
        {#if status.available}
          {scope}{status.auto_sync ? ', synced automatically' : ''}
        {:else}
          Steam is not installed on this PC.
        {/if}
      </span>
    </div>
    <Button size="sm" href="/settings/library">Settings</Button>
    <Button size="sm" icon="refresh" busy={syncing} disabled={!status.available} onclick={sync}>Sync now</Button>
  </div>
{:else if status?.available}
  <div class="source hint">
    <Icon name="gamepad" />
    <span class="text muted">Steam is installed. Turn on the Steam library to add your games with their covers.</span>
    <Button size="sm" href="/settings/library">Library settings</Button>
  </div>
{/if}

<style>
  .source {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    background: var(--panel);
    font-size: var(--text-sm);
    flex-wrap: wrap;
  }
  .text {
    display: flex;
    gap: var(--space-2);
    flex: 1;
    min-width: 200px;
    flex-wrap: wrap;
  }
</style>

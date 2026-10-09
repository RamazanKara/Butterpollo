<script lang="ts">
  // The Playnite line above the library: Playnite's state, the plugin, and Sync now.
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import { api, type PlayniteStatus } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';

  let { onsynced }: { onsynced?: () => void } = $props();

  let status = $state<PlayniteStatus | null>(null);
  let busy = $state<'' | 'sync' | 'install'>('');

  async function load() {
    try {
      status = await api.playnite.status();
    } catch {
      status = null;
    }
  }

  onMount(() => {
    void load();
  });

  const summary = $derived.by(() => {
    if (!status) return '';
    if (!status.installed) return 'The Rubylight plugin is not in Playnite yet.';
    const plugin = `plugin ${status.installed_version ?? ''}`.trim();
    if (!status.active) return `Not running · ${plugin}`;
    const games = status.game_count === 1 ? 'game' : 'games';
    return status.game_count > 0 ? `Running · ${plugin} · ${status.game_count} ${games}` : `Running · ${plugin}`;
  });

  async function sync() {
    busy = 'sync';
    try {
      const result = await api.playnite.sync();
      notify(result.changed ? 'Playnite games updated.' : 'Playnite games are up to date.', 'ok');
      onsynced?.();
      await load();
    } catch (error) {
      failed('Syncing Playnite failed', error);
    } finally {
      busy = '';
    }
  }

  async function install() {
    busy = 'install';
    try {
      const result = await api.playnite.install();
      notify(
        result.restart_required ? 'Plugin installed. Restart Playnite to load it.' : 'Plugin installed. It loads when Playnite starts.',
        'ok',
      );
      await load();
    } catch (error) {
      failed('Installing the Playnite plugin failed', error);
    } finally {
      busy = '';
    }
  }
</script>

{#if status?.enabled && (status.available || status.installed)}
  <div class="source">
    <Icon name="library" />
    <div class="text">
      <strong>Playnite</strong>
      <span class="muted">{summary}</span>
    </div>
    {#if !status.installed || status.update_available}
      <Button size="sm" icon="download" busy={busy === 'install'} disabled={busy !== ''} onclick={install}>
        {status.installed ? 'Update plugin' : 'Install plugin'}
      </Button>
    {/if}
    <Button size="sm" href="/settings/library">Settings</Button>
    <Button
      size="sm"
      icon="refresh"
      busy={busy === 'sync'}
      disabled={busy !== '' || !status.active || !status.installed}
      onclick={sync}
    >
      Sync now
    </Button>
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

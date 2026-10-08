<script lang="ts">
  import { onMount } from 'svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import CrashPanel from '../components/maintenance/CrashPanel.svelte';
  import DisplaysPanel from '../components/maintenance/DisplaysPanel.svelte';
  import HostPanel from '../components/maintenance/HostPanel.svelte';
  import LimiterPanel from '../components/maintenance/LimiterPanel.svelte';
  import PasswordPanel from '../components/maintenance/PasswordPanel.svelte';
  import SessionsPanel from '../components/maintenance/SessionsPanel.svelte';
  import UpdatesPanel from '../components/maintenance/UpdatesPanel.svelte';
  import VulkanPanel from '../components/maintenance/VulkanPanel.svelte';
  import { reason } from '../components/maintenance/util';
  import { api, type WebSession } from '../lib/api';

  // Shared by the browser list and the password form, which fills in the username.
  let sessions = $state<WebSession[] | null>(null);
  let sessionsError = $state('');
  const username = $derived(sessions?.find((session) => session.current)?.username ?? '');

  async function loadSessions() {
    try {
      sessions = (await api.auth.sessions()).sessions;
      sessionsError = '';
    } catch (error) {
      sessionsError = reason(error);
    }
  }

  onMount(loadSessions);
</script>

<div class="page">
  <PageHeader title="Maintenance" subtitle="Restart or update the host, recover displays, and manage who can sign in." />
  <div class="columns">
    <div class="stack">
      <HostPanel />
      <UpdatesPanel />
      <CrashPanel />
      <DisplaysPanel />
    </div>
    <div class="stack">
      <VulkanPanel />
      <LimiterPanel />
      <SessionsPanel {sessions} error={sessionsError} onchange={loadSessions} />
      <PasswordPanel {username} />
    </div>
  </div>
</div>

<style>
  .page {
    display: grid;
    gap: var(--space-5);
    min-width: 0;
  }
  .columns {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-5);
    align-items: start;
  }
  .stack {
    gap: var(--space-5);
  }
  @media (max-width: 1100px) {
    .columns {
      grid-template-columns: minmax(0, 1fr);
      gap: var(--space-5);
    }
  }
</style>

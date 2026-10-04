<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import LoadState from './LoadState.svelte';
  import { api, type WebSession } from '../../lib/api';
  import { confirm, failed, notify } from '../../lib/feedback.svelte';
  import { ago } from '../../lib/format';
  import { signedOut } from '../../lib/session.svelte';

  let {
    sessions,
    error,
    onchange,
  }: { sessions: WebSession[] | null; error: string; onchange: () => Promise<void> } = $props();

  let revoking = $state<string | null>(null);

  const sorted = $derived(
    [...(sessions ?? [])].sort((a, b) => Number(b.current) - Number(a.current) || b.last_seen - a.last_seen),
  );

  const label = (session: WebSession) => session.device_label || 'Unknown browser';

  async function revoke(session: WebSession) {
    const ok = await confirm(
      session.current
        ? {
            title: 'Sign out of this browser?',
            message: 'You need to sign in again to use the console here.',
            confirm: 'Sign out',
            danger: true,
          }
        : {
            title: 'Sign out this browser?',
            message: `${label(session)}${session.remote_address ? ` at ${session.remote_address}` : ''} must sign in again to use the console.`,
            confirm: 'Sign out',
            danger: true,
          },
    );
    if (!ok) return;
    revoking = session.id;
    try {
      await api.auth.revokeSession(session.id);
      if (session.current) {
        signedOut();
        return;
      }
      notify(`${label(session)} signed out.`, 'ok');
      await onchange();
    } catch (failure) {
      failed('Signing out the browser failed', failure);
    } finally {
      revoking = null;
    }
  }
</script>

<Panel title="Signed-in browsers" description="Browsers that can use this console without signing in again." flush>
  {#if sessions}
    {#if sorted.length}
      <ul>
        {#each sorted as session (session.id)}
          <li>
            <div class="who">
              <div class="name">
                <span>{label(session)}</span>
                {#if session.current}<Badge tone="accent">This browser</Badge>{/if}
              </div>
              <div class="meta muted">
                <span class="mono">{session.remote_address || 'Unknown address'}</span>
                <span aria-hidden="true">·</span>
                <span>Last seen {session.last_seen ? ago(session.last_seen) : 'never'}</span>
              </div>
            </div>
            <Button size="sm" variant="danger" busy={revoking === session.id} disabled={revoking !== null} onclick={() => revoke(session)}>
              Sign out
            </Button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="empty muted">No browsers are signed in.</p>
    {/if}
  {:else}
    <div class="pad"><LoadState {error} what="signed-in browsers" /></div>
  {/if}
</Panel>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-5);
    border-bottom: 1px solid var(--line);
  }
  li:last-child {
    border-bottom: 0;
  }
  .who {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .name {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-sm);
    font-weight: 560;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0 6px;
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .empty,
  .pad {
    padding: var(--space-4) var(--space-5);
  }
  .empty {
    font-size: var(--text-sm);
  }
</style>

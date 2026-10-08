<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import type { Role, StreamSession } from '../../lib/api';
  import { duration } from '../../lib/format';

  let {
    sessions,
    busy,
    ondisconnect,
  }: {
    sessions: StreamSession[];
    /** uuid of the device being disconnected. */
    busy: string | null;
    ondisconnect: (session: StreamSession) => void;
  } = $props();

  const ROLES: Record<Role, string> = {
    stream: 'Stream',
    remote_monitor: 'Remote monitor',
    input_only: 'Input only',
  };
</script>

<section aria-labelledby="other-connections">
  <h3 id="other-connections">Other connections</h3>
  <ul>
    {#each sessions as session, index (`${session.uuid}:${session.role}:${index}`)}
      <li>
        <div class="what">
          <span class="name">{session.device_name || 'Unnamed device'}</span>
          <Badge>{ROLES[session.role] ?? 'Other connection'}</Badge>
          {#if session.state === 'STOPPING'}<Badge tone="warn">Stopping</Badge>{/if}
        </div>
        <div class="detail muted">
          {#if session.role === 'remote_monitor'}
            <span class="num">{session.width}×{session.height} at {session.fps} fps</span>
          {/if}
          <span>Connected for <span class="num">{duration(session.uptime_seconds)}</span></span>
        </div>
        <div class="action">
          <Button
            size="sm"
            variant="ghost"
            busy={busy === session.uuid}
            disabled={session.state === 'STOPPING'}
            onclick={() => ondisconnect(session)}
          >
            Disconnect
          </Button>
        </div>
      </li>
    {/each}
  </ul>
</section>

<style>
  section {
    border-top: 1px solid var(--line);
    padding: var(--space-3) var(--space-5) var(--space-2);
  }
  h3 {
    font-size: var(--text-sm);
    color: var(--muted);
    font-weight: 560;
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    grid-template-areas:
      'what action'
      'detail action';
    align-items: center;
    gap: 2px var(--space-3);
    padding: var(--space-2) 0;
    font-size: var(--text-sm);
  }
  li + li {
    border-top: 1px solid var(--line);
  }
  .what {
    grid-area: what;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px var(--space-2);
    min-width: 0;
  }
  .name {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .detail {
    grid-area: detail;
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--space-3);
  }
  .action {
    grid-area: action;
  }
</style>

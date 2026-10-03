<script lang="ts">
  import { onMount } from 'svelte';
  import Panel from '../Panel.svelte';
  import OtpPairing from './OtpPairing.svelte';
  import PendingRequest from './PendingRequest.svelte';
  import { refreshPending, session } from '../../lib/session.svelte';

  const others = $derived.by(() => {
    const metadata = session.metadata;
    if (!metadata) return [];
    return [...new Set(metadata.pc_addresses)].filter((address) => address && address !== metadata.pc_address);
  });

  // The shell refreshes requests every 3 s; do not wait for its next turn.
  onMount(() => {
    refreshPending().catch(() => {});
  });
</script>

<Panel title="Pair a device">
  <div class="layout">
    <div class="moonlight">
      <ol class="steps">
        <li>
          <span class="step num" aria-hidden="true">1</span>
          <div class="step-body">
            <p>In Moonlight or Artemis, add this PC and select it.</p>
            {#if session.metadata}
              <dl class="facts">
                <dt>Name</dt>
                <dd>{session.metadata.host_name}</dd>
                <dt>Address</dt>
                <dd class="num">{session.metadata.pc_address}</dd>
                {#if others.length}
                  <dt>Also</dt>
                  <dd class="num">{others.join(', ')}</dd>
                {/if}
              </dl>
            {:else}
              <p class="muted small">Loading this PC's address…</p>
            {/if}
          </div>
        </li>
        <li>
          <span class="step num" aria-hidden="true">2</span>
          <div class="step-body">
            <p>It shows a 4-digit PIN. Enter that PIN here.</p>
          </div>
        </li>
      </ol>

      <div class="waiting">
        <h3>Waiting for a PIN</h3>
        {#if session.pending.length}
          <ul class="requests">
            {#each session.pending as request (request.uniqueid)}
              <PendingRequest {request} />
            {/each}
          </ul>
        {:else}
          <p class="muted small">No device is waiting to pair.</p>
        {/if}
      </div>
    </div>

    <div class="artemis">
      <OtpPairing />
    </div>
  </div>
</Panel>

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 3fr) minmax(0, 2fr);
    gap: var(--space-5) var(--space-6);
  }
  .artemis {
    padding-left: var(--space-6);
    border-left: 1px solid var(--line);
  }
  .moonlight {
    display: grid;
    gap: var(--space-5);
    align-content: start;
  }
  .steps {
    display: grid;
    gap: var(--space-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .steps li {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
  }
  .step {
    flex: none;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border: 1px solid var(--line-strong);
    border-radius: 50%;
    font-size: var(--text-xs);
    color: var(--ink-2);
  }
  .step-body {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
    padding-top: 1px;
  }
  .waiting {
    display: grid;
    gap: var(--space-2);
  }
  .requests {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
  }
  .small {
    font-size: var(--text-sm);
  }
  @media (max-width: 1000px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
    .artemis {
      padding-left: 0;
      padding-top: var(--space-5);
      border-left: 0;
      border-top: 1px solid var(--line);
    }
  }
</style>

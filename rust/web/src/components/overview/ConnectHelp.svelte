<script lang="ts">
  import Button from '../Button.svelte';
  import EmptyState from '../EmptyState.svelte';
  import type { Metadata } from '../../lib/api';

  let { metadata }: { metadata: Metadata | null } = $props();

  const others = $derived(metadata ? (metadata.pc_addresses ?? []).filter((address) => address !== metadata.pc_address) : []);
</script>

<EmptyState icon="monitor" title="Nothing is streaming">
  <ol>
    <li>Open Moonlight or Artemis on the device.</li>
    <li>
      If this PC isn't listed, add it by address:
      {#if metadata}
        <code class="address">{metadata.pc_address}</code>
        {#if others.length}
          <span class="others">
            Other addresses:
            {#each others as address, index}{index ? ', ' : ''}<code>{address}</code>{/each}. Use the one on the
            device's network.
          </span>
        {/if}
      {:else}
        <span class="muted">Loading the host address…</span>
      {/if}
    </li>
    <li>Pair with the PIN the device shows, then choose an app.</li>
  </ol>
  <Button href="/devices" icon="link" size="sm" variant={metadata?.paired_devices === 0 ? 'primary' : 'secondary'}>
    Pair a device
  </Button>
</EmptyState>

<style>
  ol {
    justify-self: stretch;
    display: grid;
    gap: var(--space-2);
    margin: var(--space-2) 0 0;
    padding-left: 1.4em;
    text-align: left;
    color: var(--ink-2);
  }
  li::marker {
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  code {
    color: var(--ink);
    overflow-wrap: anywhere;
  }
  .address {
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--sunken);
  }
  .others {
    display: block;
    margin-top: 2px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
</style>

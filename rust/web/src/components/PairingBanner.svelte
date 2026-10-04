<script lang="ts">
  import { api, type PendingPairing } from '../lib/api';
  import { failed, notify } from '../lib/feedback.svelte';
  import { refreshPending } from '../lib/session.svelte';
  import Button from './Button.svelte';
  import Icon from './Icon.svelte';

  let { requests, onpaired }: { requests: PendingPairing[]; onpaired?: () => void } = $props();

  const first = $derived(requests[0]);
  let pin = $state('');
  let busy = $state(false);

  async function pair(event: SubmitEvent) {
    event.preventDefault();
    if (!first || !/^\d{4}$/.test(pin)) return;
    busy = true;
    try {
      await api.clients.pin(pin, first.uniqueid, first.name);
      notify(`PIN sent to ${first.name}. Finish pairing on the device.`, 'ok');
      pin = '';
      await refreshPending();
      onpaired?.();
    } catch (error) {
      failed('Pairing failed', error);
    } finally {
      busy = false;
    }
  }
</script>

{#if first}
  <form class="banner" onsubmit={pair}>
    <Icon name="link" />
    <div class="text">
      <strong>{first.name} wants to pair.</strong>
      <span class="muted">Enter the PIN Moonlight shows on that device.{requests.length > 1
          ? ` ${requests.length - 1} more waiting.`
          : ''}</span>
    </div>
    <input
      class="input pin num"
      inputmode="numeric"
      autocomplete="one-time-code"
      maxlength="4"
      placeholder="PIN"
      aria-label="PIN for {first.name}"
      bind:value={pin}
    />
    <Button type="submit" variant="primary" {busy} disabled={!/^\d{4}$/.test(pin)}>Pair</Button>
  </form>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--accent-soft);
    border: 1px solid var(--accent);
    flex-wrap: wrap;
  }
  .text {
    display: grid;
    flex: 1;
    min-width: 220px;
    font-size: var(--text-sm);
  }
  .pin {
    width: 92px;
    text-align: center;
    letter-spacing: 0.3em;
    font-size: var(--text-lg);
  }
</style>

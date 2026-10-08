<script lang="ts">
  import Button from '../Button.svelte';
  import { api, type PendingPairing } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import { duration } from '../../lib/format';
  import { refreshPending } from '../../lib/session.svelte';

  let { request }: { request: PendingPairing } = $props();

  const uid = $props.id();
  let pin = $state('');
  let busy = $state(false);
  const complete = $derived(/^\d{4}$/.test(pin));

  function typed(event: Event & { currentTarget: HTMLInputElement }) {
    pin = event.currentTarget.value.replace(/\D/g, '').slice(0, 4);
    event.currentTarget.value = pin;
  }

  async function pair(event: SubmitEvent) {
    event.preventDefault();
    if (!complete) return;
    busy = true;
    try {
      await api.clients.pin(pin, request.uniqueid, request.name);
      notify(`PIN sent to ${request.name}. It shows up under paired devices once it finishes pairing.`, 'ok');
      pin = '';
      refreshPending().catch(() => {});
    } catch (error) {
      failed('Pairing failed', error);
    } finally {
      busy = false;
    }
  }
</script>

<li>
  <form class="request" onsubmit={pair}>
    <div class="who">
      <span class="name">{request.name}</span>
      <span class="muted age">Requested {duration(request.age_seconds)} ago</span>
    </div>
    <label class="visually-hidden" for="{uid}-pin">PIN shown on {request.name}</label>
    <div class="entry">
      <input
        id="{uid}-pin"
        class="input pin num"
        inputmode="numeric"
        autocomplete="one-time-code"
        maxlength="4"
        placeholder="PIN"
        value={pin}
        oninput={typed}
      />
      <Button type="submit" variant="primary" {busy} disabled={!complete}>Pair</Button>
    </div>
  </form>
</li>

<style>
  li {
    list-style: none;
  }
  .request {
    display: flex;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    flex-wrap: wrap;
    padding: var(--space-3);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: var(--accent-soft);
  }
  .who {
    display: grid;
    flex: 1 1 160px;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .age {
    font-size: var(--text-xs);
  }
  .entry {
    display: flex;
    gap: var(--space-2);
  }
  .pin {
    width: 92px;
    text-align: center;
    letter-spacing: 0.3em;
    font-size: var(--text-lg);
  }
</style>

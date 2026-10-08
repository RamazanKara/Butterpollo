<script lang="ts">
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import { api } from '../../lib/api';
  import { failed } from '../../lib/feedback.svelte';

  // The host accepts a one-time PIN for three minutes.
  const LIFETIME_MS = 180_000;

  const uid = $props.id();
  let passphrase = $state('');
  let deviceName = $state('');
  let busy = $state(false);
  let otp = $state<{ pin: string; host: string; address: string; expires: number } | null>(null);
  let now = $state(Date.now());

  const tooShort = $derived([...passphrase].length < 4);
  const left = $derived(otp ? Math.max(0, Math.ceil((otp.expires - now) / 1000)) : 0);
  const expired = $derived(left === 0);
  const countdown = $derived(`${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`);

  $effect(() => {
    if (!otp || expired) return;
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  async function create(event: SubmitEvent) {
    event.preventDefault();
    if (tooShort) return;
    busy = true;
    try {
      const result = await api.clients.otp(passphrase, deviceName.trim());
      now = Date.now();
      otp = { pin: result.otp, host: result.name, address: result.ip, expires: now + LIFETIME_MS };
    } catch (error) {
      failed('Creating a PIN failed', error);
    } finally {
      busy = false;
    }
  }
</script>

<div class="otp">
  <div>
    <h3>Pair with a one-time PIN</h3>
    <p class="muted intro">For Artemis. Create a PIN here, then enter it with the passphrase in Artemis.</p>
  </div>
  <form class="form" onsubmit={create}>
    <Field label="Passphrase" id="{uid}-passphrase" hint="At least 4 characters. You type it in Artemis too.">
      <input
        id="{uid}-passphrase"
        class="input"
        autocomplete="off"
        spellcheck="false"
        minlength="4"
        required
        bind:value={passphrase}
      />
    </Field>
    <Field label="Device name" id="{uid}-name" hint="Optional. Leave empty to use the name from Artemis.">
      <input id="{uid}-name" class="input" autocomplete="off" bind:value={deviceName} />
    </Field>
    <div>
      <Button type="submit" {busy} disabled={tooShort}>{otp ? 'Create a new PIN' : 'Create PIN'}</Button>
    </div>
  </form>

  {#if otp}
    <div class="result" class:expired aria-live="polite">
      <span class="code num" aria-label="PIN {otp.pin.split('').join(' ')}">{otp.pin}</span>
      <div class="details">
        {#if expired}
          <p><strong>This PIN expired.</strong> Create a new one.</p>
        {:else}
          <p aria-live="off">Expires in <span class="num">{countdown}</span>. Works once.</p>
          <p>In Artemis, choose “Pair with OTP” (one-time PIN) for this PC and enter this PIN and the passphrase.</p>
        {/if}
        {#if otp.host || otp.address}
          <p class="muted">
            This PC: {otp.host}{otp.host && otp.address ? ', ' : ''}<span class="num">{otp.address}</span>
          </p>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .otp {
    display: grid;
    gap: var(--space-4);
    align-content: start;
  }
  .intro {
    margin-top: 3px;
    font-size: var(--text-sm);
  }
  .form {
    display: grid;
    gap: var(--space-3);
  }
  .result {
    display: flex;
    gap: var(--space-3) var(--space-4);
    align-items: center;
    flex-wrap: wrap;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    background: var(--sunken);
  }
  .code {
    font-size: 34px;
    font-weight: 600;
    line-height: 1;
    letter-spacing: 0.18em;
  }
  .expired .code {
    color: var(--muted);
    text-decoration: line-through;
  }
  .details {
    display: grid;
    gap: 4px;
    flex: 1 1 200px;
    font-size: var(--text-sm);
  }
</style>

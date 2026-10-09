<script lang="ts">
  import Button from '../components/Button.svelte';
  import Field from '../components/Field.svelte';
  import { api } from '../lib/api';
  import { navigate } from '../lib/router.svelte';
  import { checkSession } from '../lib/session.svelte';

  let username = $state('');
  let password = $state('');
  let confirmation = $state('');
  let busy = $state(false);
  let error = $state('');

  const mismatch = $derived(confirmation.length > 0 && confirmation !== password);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (password.length < 8 || mismatch) return;
    busy = true;
    error = '';
    try {
      await api.auth.setPassword({ newUsername: username, newPassword: password, confirmNewPassword: confirmation });
      await api.auth.login(username, password, true);
      navigate('/', { replace: true });
      await checkSession();
    } catch (failure) {
      error = failure instanceof Error ? failure.message : String(failure);
    } finally {
      busy = false;
    }
  }
</script>

<div class="auth">
  <div class="brand"><span class="mark" aria-hidden="true">R</span>Rubylight</div>
  <form class="card" onsubmit={submit}>
    <h1>Set up this host</h1>
    <p class="muted">
      Create the account for this console. Set it up on this PC. Once the account is created, you can manage
      the host from other devices on your network.
    </p>
    <Field label="Username" id="new-username">
      <input id="new-username" class="input" autocomplete="username" required bind:value={username} />
    </Field>
    <Field label="Password" id="new-password" hint="At least 8 characters.">
      <input id="new-password" class="input" type="password" autocomplete="new-password" minlength="8" required bind:value={password} />
    </Field>
    <Field label="Confirm password" id="confirm-password" error={mismatch ? 'The passwords do not match.' : undefined}>
      <input id="confirm-password" class="input" type="password" autocomplete="new-password" required bind:value={confirmation} />
    </Field>
    {#if error}<p class="notice danger" role="alert">{error}</p>{/if}
    <Button type="submit" variant="primary" {busy} disabled={password.length < 8 || mismatch || !username}>Create account</Button>
  </form>
</div>

<style>
  .auth {
    min-height: 100vh;
    display: grid;
    place-content: center;
    gap: var(--space-5);
    padding: var(--space-5);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--font-display);
    font-weight: 650;
    font-size: 19px;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 7px;
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 800;
  }
  .card {
    width: min(440px, calc(100vw - 48px));
    display: grid;
    gap: var(--space-4);
    padding: var(--space-6);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
  }
  @media (max-width: 520px) {
    .card {
      padding: var(--space-5);
    }
  }
  h1 {
    font-size: var(--text-xl);
  }
</style>

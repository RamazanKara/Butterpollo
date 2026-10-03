<script lang="ts">
  import Button from '../components/Button.svelte';
  import Field from '../components/Field.svelte';
  import { api } from '../lib/api';
  import { navigate, query } from '../lib/router.svelte';
  import { checkSession } from '../lib/session.svelte';

  let username = $state('');
  let password = $state('');
  let remember = $state(true);
  let busy = $state(false);
  let error = $state('');

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = '';
    try {
      await api.auth.login(username, password, remember);
      const next = query('next');
      navigate(next && next.startsWith('/') && !next.startsWith('//') ? next : '/', { replace: true });
      await checkSession();
    } catch (failure) {
      error = failure instanceof Error ? failure.message : String(failure);
    } finally {
      busy = false;
    }
  }
</script>

<div class="auth">
  <div class="brand"><span class="mark" aria-hidden="true">B</span>Butterpollo</div>
  <form class="card" onsubmit={submit}>
    <h1>Sign in</h1>
    <p class="muted">Use the account you created when you set up this host.</p>
    <Field label="Username" id="username">
      <input id="username" class="input" autocomplete="username" required bind:value={username} />
    </Field>
    <Field label="Password" id="password">
      <input id="password" class="input" type="password" autocomplete="current-password" required bind:value={password} />
    </Field>
    <label class="remember"><input type="checkbox" bind:checked={remember} /> Keep me signed in on this browser</label>
    {#if error}<p class="notice danger" role="alert">{error}</p>{/if}
    <Button type="submit" variant="primary" {busy}>Sign in</Button>
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
    width: min(400px, calc(100vw - 40px));
    display: grid;
    gap: var(--space-4);
    padding: var(--space-6);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
  }
  h1 {
    font-size: var(--text-xl);
  }
  .remember {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: var(--text-sm);
  }
</style>

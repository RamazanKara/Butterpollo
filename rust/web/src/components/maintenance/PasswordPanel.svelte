<script lang="ts">
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import Panel from '../Panel.svelte';
  import { api } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import { signedOut } from '../../lib/session.svelte';

  /** The signed-in username, once known, fills in both username fields. */
  let { username }: { username: string } = $props();

  let currentUsername = $state('');
  let currentPassword = $state('');
  let newUsername = $state('');
  let newPassword = $state('');
  let confirmation = $state('');
  let busy = $state(false);

  let prefilled = false;
  $effect(() => {
    if (!username || prefilled) return;
    prefilled = true;
    currentUsername ||= username;
    newUsername ||= username;
  });

  const tooShort = $derived(newPassword.length > 0 && newPassword.length < 8);
  const mismatch = $derived(confirmation.length > 0 && confirmation !== newPassword);
  const ready = $derived(
    currentUsername.trim() !== '' &&
      currentPassword !== '' &&
      newUsername.trim() !== '' &&
      newPassword.length >= 8 &&
      confirmation === newPassword,
  );

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!ready) return;
    busy = true;
    try {
      await api.auth.setPassword({
        currentUsername: currentUsername.trim(),
        currentPassword,
        newUsername: newUsername.trim(),
        newPassword,
        confirmNewPassword: confirmation,
      });
      notify('Sign-in details changed. Sign in again with the new ones.', 'ok');
      signedOut();
    } catch (error) {
      failed('Changing the sign-in details failed', error);
    } finally {
      busy = false;
    }
  }
</script>

<Panel title="Change password" description="Saving signs out every browser, including this one.">
  <form class="stack" onsubmit={submit}>
    <div class="form-grid">
      <Field label="Current username" id="pw-current-username">
        <input id="pw-current-username" class="input" autocomplete="username" required bind:value={currentUsername} />
      </Field>
      <Field label="Current password" id="pw-current-password">
        <input
          id="pw-current-password"
          class="input"
          type="password"
          autocomplete="current-password"
          required
          bind:value={currentPassword}
        />
      </Field>
      <Field label="New username" id="pw-new-username" hint="Keep it the same to change only the password." wide>
        <input id="pw-new-username" class="input" autocomplete="off" required bind:value={newUsername} />
      </Field>
      <Field
        label="New password"
        id="pw-new-password"
        hint="At least 8 characters."
        error={tooShort ? 'Use at least 8 characters.' : undefined}
      >
        <input
          id="pw-new-password"
          class="input"
          type="password"
          autocomplete="new-password"
          minlength="8"
          required
          bind:value={newPassword}
        />
      </Field>
      <Field label="Confirm new password" id="pw-confirm" error={mismatch ? 'The passwords differ.' : undefined}>
        <input id="pw-confirm" class="input" type="password" autocomplete="new-password" required bind:value={confirmation} />
      </Field>
    </div>
    <div class="row">
      <Button type="submit" variant="primary" {busy} disabled={!ready}>Change password</Button>
    </div>
  </form>
</Panel>

<script lang="ts">
  import { dialog } from '../lib/feedback.svelte';
  import Button from './Button.svelte';

  let element = $state<HTMLDialogElement>();

  $effect(() => {
    if (dialog.current && element && !element.open) element.showModal();
    if (!dialog.current && element?.open) element.close();
  });
</script>

<dialog bind:this={element} oncancel={() => dialog.current?.resolve(false)} aria-labelledby="confirm-title">
  {#if dialog.current}
    <h2 id="confirm-title">{dialog.current.title}</h2>
    <p>{dialog.current.message}</p>
    <div class="buttons">
      <Button onclick={() => dialog.current?.resolve(false)}>Cancel</Button>
      <Button variant={dialog.current.danger ? 'danger' : 'primary'} onclick={() => dialog.current?.resolve(true)}>
        {dialog.current.confirm}
      </Button>
    </div>
  {/if}
</dialog>

<style>
  dialog {
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    background: var(--panel-raised);
    color: var(--ink);
    padding: var(--space-5);
    width: min(440px, calc(100vw - 32px));
    box-shadow: var(--shadow-pop);
  }
  dialog::backdrop {
    background: var(--scrim);
  }
  p {
    margin: var(--space-3) 0 var(--space-5);
    color: var(--ink-2);
    white-space: pre-line;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>

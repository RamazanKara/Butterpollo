<script lang="ts" module>
  export interface Choice {
    value: string;
    label: string;
    hint?: string;
  }
</script>

<script lang="ts">
  import Field from '../Field.svelte';
  import type { AppDraft, TextKey } from './draft.svelte';

  let {
    draft,
    key,
    label,
    hint,
    options,
    empty = 'Use the host setting',
  }: {
    draft: AppDraft;
    key: TextKey;
    label: string;
    /** Shown when the selected option has no hint of its own. */
    hint?: string;
    options: Choice[];
    /** Label of the empty choice, which removes the key. */
    empty?: string;
  } = $props();

  const id = $props.id();
  const value = $derived(draft.text(key));
  const known = $derived(options.find((option) => option.value === value));
</script>

<Field {label} {id} hint={known?.hint ?? hint}>
  <select {id} class="select" bind:value={() => draft.text(key), (next) => draft.setText(key, next)}>
    <option value="">{empty}</option>
    {#if value && !known}
      <option {value}>Saved custom choice</option>
    {/if}
    {#each options as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
</Field>

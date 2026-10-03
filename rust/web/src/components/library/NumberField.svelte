<script lang="ts">
  import Field from '../Field.svelte';
  import { NUMBERS, type AppDraft, type NumberKey } from './draft.svelte';

  let {
    draft,
    key,
    label,
    hint,
    placeholder,
    unit = '',
  }: {
    draft: AppDraft;
    key: NumberKey;
    label: string;
    hint?: string;
    /** Shown when empty: the value the host uses then. */
    placeholder: string;
    unit?: string;
  } = $props();

  const id = $props.id();
  const error = $derived(draft.numberError(key));
  const range = $derived(NUMBERS[key]);
</script>

<Field {label} {id} {hint} error={error || undefined}>
  <div class="number">
    <input
      {id}
      class="input num"
      type="number"
      step="1"
      min={range?.min}
      max={range?.max}
      {placeholder}
      aria-invalid={error ? 'true' : undefined}
      bind:value={() => draft.number(key), (value) => draft.setNumber(key, value)}
    />
    {#if unit}<span class="unit muted">{unit}</span>{/if}
  </div>
</Field>

<style>
  .number {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .input {
    max-width: 150px;
  }
  .unit {
    font-size: var(--text-sm);
  }
</style>

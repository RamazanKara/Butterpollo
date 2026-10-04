<script lang="ts">
  import Button from '../Button.svelte';
  import Toggle from '../Toggle.svelte';
  import NumberField from './NumberField.svelte';
  import { api } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import type { AppDraft, NumberKey } from './draft.svelte';

  let { draft, running }: { draft: AppDraft; running: boolean } = $props();

  const TUNING: NumberKey[] = [
    'rtx-hdr-sdr-brightness',
    'rtx-hdr-contrast',
    'rtx-hdr-saturation',
    'rtx-hdr-middle-gray',
    'rtx-hdr-peak-brightness',
  ];

  let applying = $state(false);
  const uuid = $derived(draft.saved.uuid);
  const invalid = $derived(TUNING.some((key) => draft.numberError(key) !== ''));

  async function apply() {
    if (!uuid) return;
    // Only what this app sets; the rest follows the host.
    const overrides: Record<string, string | number | boolean> = {};
    if ('rtx-hdr' in draft.app) overrides.rtx_hdr = draft.flag('rtx-hdr');
    for (const key of TUNING) {
      const value = draft.number(key);
      if (value !== undefined) overrides[key.replaceAll('-', '_')] = value;
    }
    applying = true;
    try {
      const result = await api.apps.rtxLive(uuid, overrides);
      if (result.applied) notify('Applied to the running app. Save to keep these values.', 'ok');
      else notify('Nothing changed: the app already uses these values, or it is no longer running.');
    } catch (error) {
      failed('Applying RTX HDR failed', error);
    } finally {
      applying = false;
    }
  }
</script>

<div class="rtx">
  <Toggle
    label="Use RTX HDR for this app"
    hint="Turns the app’s SDR picture into HDR on NVIDIA GPUs when a device streams in HDR."
    bind:checked={() => draft.flag('rtx-hdr'), (value) => draft.setFlag('rtx-hdr', value)}
  />

  <div class="numbers">
    <NumberField {draft} key="rtx-hdr-sdr-brightness" label="SDR brightness" hint="0 to 100." placeholder="Host" />
    <NumberField {draft} key="rtx-hdr-contrast" label="Contrast" hint="−100 to 100." placeholder="Host" />
    <NumberField {draft} key="rtx-hdr-saturation" label="Saturation" hint="−100 to 100." placeholder="Host" />
    <NumberField {draft} key="rtx-hdr-middle-gray" label="Middle gray" hint="10 to 100." placeholder="Host" />
    <NumberField
      {draft}
      key="rtx-hdr-peak-brightness"
      label="Peak brightness"
      hint="400 to 2000."
      placeholder="Host"
      unit="nits"
    />
  </div>
  <p class="note">Empty fields use the host setting.</p>

  <div class="live">
    <Button busy={applying} disabled={!uuid || !running || invalid} onclick={apply}>Apply to the running app</Button>
    <span class="muted note">
      {running ? 'Tries the values above now, without saving them.' : 'Available while this app is running.'}
    </span>
  </div>
</div>

<style>
  .rtx {
    display: grid;
    gap: var(--space-4);
  }
  .numbers {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 170px), 1fr));
    gap: var(--space-4) var(--space-5);
  }
  .note {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .live {
    display: flex;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    flex-wrap: wrap;
    padding-top: var(--space-4);
    border-top: 1px solid var(--line);
  }
</style>

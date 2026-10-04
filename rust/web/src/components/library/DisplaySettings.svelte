<script lang="ts">
  import Field from '../Field.svelte';
  import Toggle from '../Toggle.svelte';
  import ChoiceField, { type Choice } from './ChoiceField.svelte';
  import NumberField from './NumberField.svelte';
  import type { AppDraft, FlagKey } from './draft.svelte';

  let { draft }: { draft: AppDraft } = $props();

  const uid = $props.id();

  const MODES: Choice[] = [
    { value: 'disabled', label: 'Off, stream a monitor' },
    { value: 'per_client', label: 'One for each device' },
    { value: 'shared', label: 'One shared by all devices' },
  ];

  const LAYOUTS: Choice[] = [
    { value: 'exclusive', label: 'Exclusive', hint: 'Turns the other monitors off while streaming.' },
    { value: 'extended', label: 'Extended', hint: 'Adds the virtual display next to the monitors.' },
    { value: 'extended_primary', label: 'Extended, primary', hint: 'Adds it and makes it the main display.' },
    {
      value: 'extended_isolated',
      label: 'Extended, isolated',
      hint: 'Adds it far from the monitors so the mouse can’t wander onto it.',
    },
    {
      value: 'extended_primary_isolated',
      label: 'Extended, primary and isolated',
      hint: 'Main display, placed far from the other monitors.',
    },
  ];

  const ACTIVATION: Choice[] = [
    { value: 'verify_only', label: 'Only check that it’s on' },
    { value: 'ensure_active', label: 'Turn it on' },
    { value: 'ensure_primary', label: 'Turn it on and make it primary' },
    { value: 'ensure_only_display', label: 'Turn it on and the others off' },
    { value: 'disabled', label: 'Leave the monitors alone' },
  ];
</script>

{#snippet flag(key: FlagKey, label: string, hint?: string)}
  <Toggle {label} {hint} bind:checked={() => draft.flag(key), (value) => draft.setFlag(key, value)} />
{/snippet}

<div class="display">
  <div class="toggles">
    {@render flag(
      'virtual-display',
      'Use a virtual display',
      'Streams a virtual display sized for the device, even when the host streams a monitor.',
    )}
    {@render flag('virtual-display-primary', 'Make the virtual display primary', 'Uses the Extended, primary layout.')}
    {@render flag(
      'use-app-identity',
      'Keep one display identity for this app',
      'Windows remembers the virtual display’s resolution and layout for this app.',
    )}
    {@render flag(
      'per-client-app-identity',
      'Keep a separate identity for each device',
      'Only with the identity above turned on.',
    )}
  </div>

  <div class="form-grid">
    <ChoiceField
      {draft}
      key="virtual-display-mode"
      label="Virtual display mode"
      hint="A device’s own setting wins over this."
      options={MODES}
    />
    <ChoiceField
      {draft}
      key="virtual-display-layout"
      label="Virtual display layout"
      hint="Where the virtual display sits next to the monitors."
      options={LAYOUTS}
    />
    <Field
      label="Display output"
      id="{uid}-output"
      hint="Device ID or name of the monitor to stream. Empty uses the device’s or the host’s choice."
    >
      <input
        id="{uid}-output"
        class="input mono"
        autocomplete="off"
        spellcheck="false"
        placeholder="Host setting"
        bind:value={() => draft.text('display-output'), (value) => draft.setText('display-output', value)}
      />
    </Field>
    <ChoiceField
      {draft}
      key="dd-configuration-option"
      label="Monitor setup"
      hint="What happens to the monitor when the stream doesn’t use a virtual display."
      options={ACTIVATION}
    />
    <NumberField
      {draft}
      key="scale-factor"
      label="Render scale"
      hint="Renders the app at this share of the stream resolution. 100 follows the device."
      placeholder="100"
      unit="%"
    />
  </div>
</div>

<style>
  .display {
    display: grid;
    gap: var(--space-5);
  }
  .toggles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: var(--space-4) var(--space-5);
  }
</style>

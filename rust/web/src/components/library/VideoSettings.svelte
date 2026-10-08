<script lang="ts">
  import Field from '../Field.svelte';
  import Toggle from '../Toggle.svelte';
  import ChoiceField, { type Choice } from './ChoiceField.svelte';
  import type { AppDraft } from './draft.svelte';

  let { draft }: { draft: AppDraft } = $props();

  const uid = $props.id();

  const GENERATION: Choice[] = [
    { value: 'game-provided', label: 'Provided by the game', hint: 'The game adds generated frames, e.g. DLSS or FSR.' },
    {
      value: 'nvidia-smooth-motion',
      label: 'NVIDIA Smooth Motion',
      hint: 'The NVIDIA driver adds generated frames.',
    },
    {
      value: 'lossless-scaling',
      label: 'Lossless Scaling',
      hint: 'Lossless Scaling adds generated frames; the game is limited to half the target. See Lossless Scaling below.',
    },
  ];

  const GAMEPADS: Choice[] = [
    { value: 'auto', label: 'Automatic' },
    { value: 'vhf_xbox_one', label: 'Xbox One' },
    { value: 'vhf_xbox', label: 'Xbox Series' },
    { value: 'vhf_ds4', label: 'DualShock 4' },
    { value: 'vhf_ds5', label: 'DualSense' },
    { value: 'vhf_switch', label: 'Switch Pro' },
  ];
</script>

<div class="video">
  <div class="form-grid">
    <Field
      label="Prefer 10-bit SDR"
      id="{uid}-sdr"
      hint="When on, HDR requests stream as 10-bit SDR and the display stays in SDR. Needs HEVC or AV1."
    >
      <select
        id="{uid}-sdr"
        class="select"
        bind:value={() => draft.tristate('prefer-10bit-sdr'), (value) => draft.setTristate('prefer-10bit-sdr', value)}
      >
        <option value="">Use the host setting</option>
        <option value="on">On</option>
        <option value="off">Off</option>
      </select>
    </Field>
    <ChoiceField
      {draft}
      key="frame-generation-mode"
      label="Frame generation"
      hint="Tells the host when frames are generated, so the stream keeps pace."
      empty="None"
      options={GENERATION}
    />
    <ChoiceField
      {draft}
      key="gamepad"
      label="Controller type"
      hint="Automatic matches each device’s controller: DualSense for PlayStation or for motion and touchpad input, Switch Pro for Nintendo, Xbox Series otherwise. A choice here uses that controller for this app."
      options={GAMEPADS}
    />
  </div>

  <div class="toggles">
    <Toggle
      label="Legacy frame generation capture fix"
      hint="Compatibility option for older frame generation setups. Only used when Frame generation is set to None."
      bind:checked={() => draft.flag('gen1-framegen-fix'), (value) => draft.setFlag('gen1-framegen-fix', value)}
    />
    <Toggle
      label="Frame generation limiter fix"
      bind:checked={() => draft.flag('frame-gen-limiter-fix'), (value) => draft.setFlag('frame-gen-limiter-fix', value)}
    />
  </div>
</div>

<style>
  .video {
    display: grid;
    gap: var(--space-5);
  }
  .toggles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: var(--space-4) var(--space-5);
  }
</style>

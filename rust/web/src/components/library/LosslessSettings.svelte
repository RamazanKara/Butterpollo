<script lang="ts">
  // Lossless Scaling for this app: scaling and frame generation in the profile
  // the host writes into Lossless Scaling while the app runs.
  import Field from '../Field.svelte';
  import Toggle from '../Toggle.svelte';
  import ChoiceField, { type Choice } from './ChoiceField.svelte';
  import NumberField from './NumberField.svelte';
  import type { AppDraft } from './draft.svelte';

  let { draft }: { draft: AppDraft } = $props();

  const uid = $props.id();

  const PROFILES: Choice[] = [
    {
      value: 'recommended',
      label: 'Recommended',
      hint: 'Windows Graphics Capture, HDR, adaptive frame generation and performance mode.',
    },
    { value: 'custom', label: 'Custom', hint: 'Your Lossless Scaling defaults, with the choices below.' },
  ];
  const SCALING = [
    { value: 'off', label: 'Off' },
    { value: 'ls1', label: 'LS1' },
    { value: 'fsr', label: 'AMD FSR' },
    { value: 'nis', label: 'NVIDIA Image Scaling' },
    { value: 'sgsr', label: 'Snapdragon GSR' },
    { value: 'bcas', label: 'Bicubic CAS' },
    { value: 'anime4k', label: 'Anime4K' },
    { value: 'xbr', label: 'xBR' },
    { value: 'sharp-bilinear', label: 'Sharp bilinear' },
    { value: 'integer', label: 'Integer' },
    { value: 'nearest', label: 'Nearest neighbour' },
  ];

  const generating = $derived(draft.text('frame-generation-mode') === 'lossless-scaling');
  const used = $derived(draft.flag('lossless-scaling-enabled') || generating);
  const recommended = $derived(draft.losslessProfile() === 'lossless-scaling-recommended');
  const scaling = $derived(String(draft.lossless('scaling-type') ?? 'off').toLowerCase());
  const sharpens = $derived(['ls1', 'fsr', 'nis', 'sgsr'].includes(scaling));

  function numeric(field: string): number | undefined {
    const value = draft.lossless(field);
    const number = typeof value === 'number' ? value : typeof value === 'string' ? Number(value) : NaN;
    return Number.isFinite(number) ? number : undefined;
  }
  function performance(): boolean {
    const value = draft.lossless('performance-mode');
    if (typeof value === 'boolean') return value;
    if (typeof value === 'string') return ['true', '1', 'yes'].includes(value.toLowerCase());
    return recommended;
  }
</script>

<div class="lossless">
  <div class="toggles">
    <Toggle
      label="Scale with Lossless Scaling"
      hint="While the app runs, Lossless Scaling scales the game. Choose Lossless Scaling under Frame generation for generated frames."
      bind:checked={() => draft.flag('lossless-scaling-enabled'), (value) => draft.setFlag('lossless-scaling-enabled', value)}
    />
  </div>

  {#if used}
    <div class="form-grid">
      <ChoiceField
        {draft}
        key="lossless-scaling-profile"
        label="Profile"
        empty="Custom"
        options={PROFILES.filter((p) => p.value !== 'custom')}
        hint="Custom uses your Lossless Scaling defaults, with the choices below."
      />
      <Field label="Upscaling" id="{uid}-scaling">
        <select
          id="{uid}-scaling"
          class="select"
          bind:value={() => scaling, (value) => draft.setLossless('scaling-type', value === 'off' ? undefined : value)}
        >
          {#each SCALING as mode (mode.value)}
            <option value={mode.value}>{mode.label}</option>
          {/each}
        </select>
      </Field>
      {#if scaling !== 'off'}
        <Field label="Render resolution" id="{uid}-resolution" hint="Percent of the output resolution the game renders at, 10 to 100.">
          <input
            id="{uid}-resolution"
            class="input num"
            type="number"
            min="10"
            max="100"
            step="5"
            placeholder="100"
            bind:value={() => numeric('resolution-scale'), (value) => draft.setLossless('resolution-scale', value ?? undefined)}
          />
        </Field>
      {/if}
      {#if sharpens}
        <Field label="Sharpening" id="{uid}-sharpening" hint="1 to 10, default 5.">
          <input
            id="{uid}-sharpening"
            class="input num"
            type="number"
            min="1"
            max="10"
            placeholder="5"
            bind:value={() => numeric('sharpening'), (value) => draft.setLossless('sharpening', value ?? undefined)}
          />
        </Field>
      {/if}
      {#if generating}
        <Field label="Flow scale" id="{uid}-flow" hint="Motion estimation resolution, 0 to 100 percent, default 50.">
          <input
            id="{uid}-flow"
            class="input num"
            type="number"
            min="0"
            max="100"
            placeholder="50"
            bind:value={() => numeric('flow-scale'), (value) => draft.setLossless('flow-scale', value ?? undefined)}
          />
        </Field>
        <NumberField
          {draft}
          key="lossless-scaling-target-fps"
          label="Target frame rate"
          hint="The frame rate Lossless Scaling generates up to. Empty uses the stream's."
          placeholder="Stream's"
          unit="fps"
        />
        <NumberField
          {draft}
          key="lossless-scaling-rtss-limit"
          label="Game frame limit"
          hint="The game is limited to this while frames are generated. Empty uses half the target."
          placeholder="Half"
          unit="fps"
        />
      {/if}
      <NumberField
        {draft}
        key="lossless-scaling-launch-delay"
        label="Start delay"
        hint="Seconds to wait after the game's window appears, 0 to 600."
        placeholder="8"
        unit="s"
      />
    </div>
    <div class="toggles">
      <Toggle
        label="Performance mode"
        hint="Lighter frame generation for weaker GPUs."
        bind:checked={performance, (value) => draft.setLossless('performance-mode', value)}
      />
      <Toggle
        label="Let Lossless Scaling start by itself"
        hint="Uses Lossless Scaling's auto scale instead of pressing its hotkey. Overrides the host setting."
        bind:checked={() => draft.flag('lossless-scaling-legacy-auto-detect'),
        (value) => draft.setFlag('lossless-scaling-legacy-auto-detect', value)}
      />
    </div>
  {/if}
</div>

<style>
  .lossless {
    display: grid;
    gap: var(--space-5);
  }
  .toggles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: var(--space-4) var(--space-5);
  }
</style>

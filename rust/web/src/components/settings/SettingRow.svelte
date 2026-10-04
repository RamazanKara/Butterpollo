<script lang="ts">
  import Field from '../Field.svelte';
  import SettingControl from './SettingControl.svelte';
  import type { ConfigValue } from '../../lib/api';
  import type { Setting } from '../../lib/settings-types';
  import { describeDefault } from './values';

  let {
    setting,
    value,
    stored,
    changed,
    resetting,
    inactive = false,
    invalidText,
    onchange,
    onreset,
    onundo,
    oninvalid,
  }: {
    setting: Setting;
    /** The effective value, normalised for the control. */
    value: ConfigValue;
    /** The host has a value for this key. */
    stored: boolean;
    /** Saving would change what the host stores. */
    changed: boolean;
    /** Reset to default is waiting to be saved. */
    resetting: boolean;
    /** Hidden by the current choices; only search shows it. */
    inactive?: boolean;
    invalidText?: string;
    onchange: (value: ConfigValue) => void;
    onreset: () => void;
    onundo: () => void;
    oninvalid: (text: string | null) => void;
  } = $props();

  const uid = $props.id();
  const id = `${uid}-control`;
  const kind = $derived(setting.control.kind);
  const grouped = $derived(kind === 'list' || kind === 'commands' || kind === 'server-commands');
  /** Unsaved: a change, or JSON that does not parse yet. Either can be undone. */
  const marked = $derived(changed || invalidText !== undefined);
  const fallback = $derived(describeDefault(setting));
</script>

{#snippet control()}
  <!-- Not class={kind}: "select" would pick up the global form style. -->
  <div class="control" data-kind={kind}>
    <SettingControl {setting} {id} {value} {onchange} {invalidText} {oninvalid} />
  </div>
{/snippet}

<div class="setting" class:marked>
  {#if kind === 'toggle'}
    {@render control()}
  {:else if grouped}
    <fieldset>
      <legend>{setting.label}</legend>
      {#if setting.description}<p class="description">{setting.description}</p>{/if}
      {@render control()}
    </fieldset>
  {:else}
    <Field label={setting.label} {id}>
      {#if setting.description}<p class="description">{setting.description}</p>{/if}
      {@render control()}
    </Field>
  {/if}

  <div class="meta">
    <code class="key" title="Configuration key">{setting.key}</code>
    <span>Default: <span class="default" class:mono={fallback.literal}>{fallback.text}</span></span>
    {#if setting.restart}<span>Applies after a restart</span>{/if}
    {#if inactive}<span>Not used with the current settings</span>{/if}
    {#if marked}<span class="changed">Changed</span>{/if}
    {#if resetting}<span>Resets to the default when saved</span>{/if}
    {#if marked}
      <button type="button" class="link" onclick={onundo}>
        Undo<span class="visually-hidden">{` ${setting.label}`}</span>
      </button>
    {/if}
    {#if stored && !resetting}
      <button type="button" class="link" onclick={onreset}>
        Reset to default<span class="visually-hidden">{` for ${setting.label}`}</span>
      </button>
    {/if}
  </div>
</div>

<style>
  .setting {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    min-width: 0;
  }
  .marked {
    box-shadow: inset 3px 0 0 var(--accent);
  }
  fieldset {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }
  legend {
    padding: 0;
    margin-bottom: 6px;
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .description {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.45;
    max-width: 75ch;
  }
  .control {
    display: grid;
    gap: 6px;
    min-width: 0;
    max-width: 440px;
  }
  .control[data-kind='list'] {
    max-width: 560px;
  }
  .control[data-kind='commands'],
  .control[data-kind='server-commands'],
  .control[data-kind='json'],
  .control[data-kind='toggle'] {
    max-width: 760px;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px var(--space-3);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .key {
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .default {
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .changed {
    color: var(--ink);
    font-weight: 600;
  }
  .link {
    border: 0;
    padding: 0;
    background: none;
    color: var(--ink-2);
    font-size: var(--text-xs);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .link:hover {
    color: var(--ink);
  }
  @media (max-width: 520px) {
    .setting {
      padding: var(--space-3) var(--space-4);
    }
  }
</style>

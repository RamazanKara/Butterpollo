<script lang="ts">
  import { tick, untrack } from 'svelte';
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import PathPicker from '../PathPicker.svelte';
  import Toggle from '../Toggle.svelte';
  import type { ConfigValue, PrepCommand } from '../../lib/api';
  import type { Option, Setting } from '../../lib/settings-types';
  import { session } from '../../lib/session.svelte';
  import {
    jsonText,
    parseJson,
    sameValue,
    toCommands,
    toList,
    toNumber,
    toServerCommands,
    toText,
    type ServerCommand,
  } from './values';

  let {
    setting,
    id,
    value,
    onchange,
    invalidText,
    oninvalid,
  }: {
    setting: Setting;
    /** On the control the row's label points at. */
    id: string;
    /** The effective value, normalised for this control. */
    value: ConfigValue;
    onchange: (value: ConfigValue) => void;
    /** JSON text that does not parse yet. The page keeps it so it survives a remount. */
    invalidText?: string;
    /** Called with the text while it is not valid JSON, and with null once it is. */
    oninvalid?: (text: string | null) => void;
  } = $props();

  const control = $derived(setting.control);
  const label = $derived(setting.label);
  const current = $derived(toText(value));

  // ---------- choices: select, display, audio, adapter ----------

  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

  const choices = $derived.by((): Option[] => {
    const meta = session.metadata;
    const options: Option[] = [];
    switch (control.kind) {
      case 'select':
        if (!control.options.some((option) => option.value === '') && (current === '' || toText(setting.default) === '')) {
          options.push({ value: '', label: 'Not set' });
        }
        options.push(...control.options);
        break;
      case 'display':
        options.push({ value: '', label: 'Automatic' });
        for (const display of meta?.capture_status.displays ?? []) {
          const name = display.display_name;
          if (!name) continue;
          // The host also matches the device id; keep the stored spelling selected.
          const matches = current !== '' && [name, display.device_id].some((field) => field && same(field, current));
          const choice = matches ? current : name;
          if (options.some((option) => option.value === choice)) continue;
          const text = display.friendly_name ? `${display.friendly_name} (${name})` : name;
          options.push({ value: choice, label: display.primary ? `${text} · primary` : text });
        }
        break;
      case 'audio':
        options.push({ value: '', label: 'System default' });
        for (const sink of meta?.audio_sinks ?? []) {
          // The host matches the id, name, description or adapter name.
          const matches =
            current !== '' && [sink.id, sink.name, sink.description, sink.adapter].some((field) => field && same(field, current));
          const choice = matches ? current : sink.id;
          if (!choice || options.some((option) => option.value === choice)) continue;
          const marks: string[] = [];
          if (sink.default) marks.push('default');
          if (sink.virtual_sink && !sink.name.toLowerCase().includes('steam streaming speakers')) {
            marks.push('Steam Streaming Speakers');
          }
          options.push({ value: choice, label: [sink.name || sink.id, ...marks].join(' · ') });
        }
        break;
      case 'adapter': {
        options.push({ value: '', label: 'Automatic' });
        const names = new Set((meta?.capture_status.displays ?? []).map((display) => display.adapter).filter(Boolean));
        for (const name of names) options.push({ value: current && same(current, name) ? current : name, label: name });
        break;
      }
      default:
        return [];
    }
    if (!options.some((option) => option.value === current)) {
      const missing = { value: current, label: current === '' ? 'Not set' : `${current} (current)` };
      options.splice(options[0]?.value === '' ? 1 : 0, 0, missing);
    }
    return options;
  });

  const listsHost = $derived(control.kind === 'display' || control.kind === 'audio' || control.kind === 'adapter');

  const hostError = $derived.by(() => {
    const meta = session.metadata;
    if (!meta) return '';
    if (control.kind === 'display' || control.kind === 'adapter') {
      return meta.capture_status.error ? `Could not list displays: ${meta.capture_status.error}` : '';
    }
    if (control.kind === 'audio') return meta.audio_error ? `Could not list audio devices: ${meta.audio_error}` : '';
    return '';
  });

  // ---------- number ----------

  // What is typed, so a half-typed number is not replaced while editing.
  let numberText = $state(untrack(() => (typeof value === 'number' ? String(value) : '')));

  $effect(() => {
    const next = control.kind === 'number' && typeof value === 'number' ? value : null;
    untrack(() => {
      const shown = numberText.trim() === '' ? null : toNumber(numberText);
      if (shown !== next) numberText = next === null ? '' : String(next);
    });
  });

  function editNumber(text: string) {
    numberText = text;
    onchange(toNumber(text) ?? '');
  }

  const rangeError = $derived.by(() => {
    if (control.kind !== 'number' || typeof value !== 'number') return '';
    const { min, max } = control;
    if ((min !== undefined && value < min) || (max !== undefined && value > max)) {
      if (min !== undefined && max !== undefined) return `Enter a number from ${min} to ${max}.`;
      return min !== undefined ? `Enter ${min} or more.` : `Enter ${max} or less.`;
    }
    return '';
  });

  // ---------- rows: list, commands, server-commands ----------

  const items = $derived(control.kind === 'list' ? toList(value) : []);
  const commands = $derived(control.kind === 'commands' ? toCommands(value) : []);
  const serverCommands = $derived(control.kind === 'server-commands' ? toServerCommands(value) : []);

  let rowsElement = $state<HTMLElement>();

  async function addRow(next: unknown[]) {
    onchange(next);
    await tick();
    const rows = rowsElement?.querySelectorAll('[data-row]');
    rows?.[rows.length - 1]?.querySelector('input')?.focus();
  }

  const without = <T,>(list: T[], index: number) => list.filter((_, at) => at !== index);

  function setItem(index: number, text: string) {
    onchange(items.map((item, at) => (at === index ? text : item)));
  }
  function setCommand(index: number, change: Partial<PrepCommand>) {
    onchange(commands.map((row, at) => (at === index ? { ...row, ...change } : row)));
  }
  function setServerCommand(index: number, change: Partial<ServerCommand>) {
    onchange(serverCommands.map((row, at) => (at === index ? { ...row, ...change } : row)));
  }

  // ---------- json ----------

  let jsonDraft = $state(untrack(() => invalidText ?? jsonText(value)));
  const jsonError = $derived.by(() => {
    if (control.kind !== 'json') return '';
    const parsed = parseJson(jsonDraft);
    return parsed.ok ? '' : parsed.error;
  });
  const jsonRows = $derived(Math.min(16, Math.max(4, jsonDraft.split('\n').length + 1)));

  // Follow resets and discards; leave the text alone while it says the same thing.
  $effect(() => {
    const next = value;
    const pending = invalidText;
    untrack(() => {
      if (control.kind !== 'json') return;
      if (pending !== undefined) {
        if (pending !== jsonDraft) jsonDraft = pending;
        return;
      }
      const parsed = parseJson(jsonDraft);
      if (!parsed.ok || !sameValue(parsed.value, next ?? '')) jsonDraft = jsonText(next);
    });
  });

  function editJson(text: string) {
    jsonDraft = text;
    const parsed = parseJson(text);
    if (parsed.ok) {
      oninvalid?.(null);
      onchange(parsed.value);
    } else {
      oninvalid?.(text);
    }
  }
</script>

{#if control.kind === 'toggle'}
  <Toggle
    label={setting.label}
    hint={setting.description || undefined}
    checked={value === true}
    onchange={(checked) => onchange(checked)}
  />
{:else if control.kind === 'select' || listsHost}
  <select {id} class="select" onchange={(event) => onchange(event.currentTarget.value)}>
    {#each choices as option (option.value)}
      <option value={option.value} selected={option.value === current}>{option.label}</option>
    {/each}
    {#if listsHost && session.metadata === null}
      <option disabled value={'\u0000loading'}>Loading…</option>
    {/if}
  </select>
  {#if hostError}<p class="note">{hostError}</p>{/if}
{:else if control.kind === 'number'}
  <div class="number">
    <input
      {id}
      class="input num"
      type="number"
      inputmode="decimal"
      min={control.min}
      max={control.max}
      step={control.step ?? 'any'}
      placeholder={toNumber(setting.default)?.toString()}
      aria-invalid={rangeError ? 'true' : undefined}
      aria-describedby={rangeError ? `${id}-range` : undefined}
      value={numberText}
      oninput={(event) => editNumber(event.currentTarget.value)}
    />
    {#if control.unit}<span class="unit muted">{control.unit}</span>{/if}
  </div>
  {#if rangeError}<p class="error" id="{id}-range">{rangeError}</p>{/if}
{:else if control.kind === 'text'}
  {#snippet text()}
    <input
      {id}
      class="input"
      class:mono={control.mono}
      type="text"
      autocomplete="off"
      spellcheck={control.mono ? 'false' : undefined}
      placeholder={control.placeholder ?? (toText(setting.default) || undefined)}
      value={current}
      oninput={(event) => onchange(event.currentTarget.value)}
    />
  {/snippet}
  {#if control.browse}
    <PathPicker kind={control.browse} value={current} onpick={onchange} children={text} />
  {:else}
    {@render text()}
  {/if}
{:else if control.kind === 'list'}
  <div class="rows" {id} bind:this={rowsElement}>
    {#each items as item, index}
      <div class="item" data-row>
        <input
          class="input mono"
          aria-label="{label}, item {index + 1}"
          autocomplete="off"
          spellcheck="false"
          placeholder={control.placeholder}
          value={item}
          oninput={(event) => setItem(index, event.currentTarget.value)}
        />
        <button
          type="button"
          class="remove"
          aria-label="Remove item {index + 1}"
          title="Remove"
          onclick={() => onchange(without(items, index))}
        >
          <Icon name="x" size={15} />
        </button>
      </div>
    {:else}
      <p class="none">None.</p>
    {/each}
    <div><Button size="sm" icon="plus" onclick={() => addRow([...items, ''])}>Add</Button></div>
  </div>
{:else if control.kind === 'commands'}
  <div class="rows" {id} bind:this={rowsElement}>
    {#each commands as row, index}
      <div class="command" data-row>
        <label class="cell">
          <span class="cell-label">Do<span class="visually-hidden">, command {index + 1}</span></span>
          <input
            class="input mono"
            autocomplete="off"
            spellcheck="false"
            value={row.do}
            oninput={(event) => setCommand(index, { do: event.currentTarget.value })}
          />
        </label>
        <label class="cell">
          <span class="cell-label">Undo<span class="visually-hidden">, command {index + 1}</span></span>
          <input
            class="input mono"
            autocomplete="off"
            spellcheck="false"
            value={row.undo}
            oninput={(event) => setCommand(index, { undo: event.currentTarget.value })}
          />
        </label>
        <div class="command-foot">
          <label class="check">
            <input
              type="checkbox"
              checked={row.elevated}
              onchange={(event) => setCommand(index, { elevated: event.currentTarget.checked })}
            />
            As administrator
          </label>
          <button
            type="button"
            class="remove"
            aria-label="Remove command {index + 1}"
            title="Remove"
            onclick={() => onchange(without(commands, index))}
          >
            <Icon name="x" size={15} />
          </button>
        </div>
      </div>
    {:else}
      <p class="none">None.</p>
    {/each}
    <div>
      <Button size="sm" icon="plus" onclick={() => addRow([...commands, { do: '', undo: '', elevated: false }])}>
        Add command
      </Button>
    </div>
  </div>
{:else if control.kind === 'server-commands'}
  <div class="rows" {id} bind:this={rowsElement}>
    {#each serverCommands as row, index}
      <div class="command" data-row>
        <label class="cell">
          <span class="cell-label">Name<span class="visually-hidden">, command {index + 1}</span></span>
          <input
            class="input"
            autocomplete="off"
            value={row.name}
            oninput={(event) => setServerCommand(index, { name: event.currentTarget.value })}
          />
        </label>
        <label class="cell">
          <span class="cell-label">Command<span class="visually-hidden">{` ${index + 1}`}</span></span>
          <input
            class="input mono"
            autocomplete="off"
            spellcheck="false"
            value={row.cmd}
            oninput={(event) => setServerCommand(index, { cmd: event.currentTarget.value })}
          />
        </label>
        <div class="command-foot">
          <label class="check">
            <input
              type="checkbox"
              checked={row.elevated}
              onchange={(event) => setServerCommand(index, { elevated: event.currentTarget.checked })}
            />
            As administrator
          </label>
          <button
            type="button"
            class="remove"
            aria-label="Remove command {index + 1}"
            title="Remove"
            onclick={() => onchange(without(serverCommands, index))}
          >
            <Icon name="x" size={15} />
          </button>
        </div>
      </div>
    {:else}
      <p class="none">None.</p>
    {/each}
    <div>
      <Button size="sm" icon="plus" onclick={() => addRow([...serverCommands, { name: '', cmd: '', elevated: false }])}>
        Add command
      </Button>
    </div>
  </div>
{:else if control.kind === 'json'}
  <textarea
    {id}
    class="textarea"
    rows={jsonRows}
    spellcheck="false"
    autocomplete="off"
    aria-invalid={jsonError ? 'true' : undefined}
    aria-describedby={jsonError ? `${id}-json` : undefined}
    value={jsonDraft}
    oninput={(event) => editJson(event.currentTarget.value)}
  ></textarea>
  {#if jsonError}<p class="error" id="{id}-json">Not valid JSON: {jsonError}</p>{/if}
{/if}

<style>
  .note,
  .error,
  .none {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
  }
  .error {
    color: var(--danger);
  }
  [aria-invalid='true'] {
    border-color: var(--danger);
  }
  .number {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .number .input {
    max-width: 160px;
  }
  .unit {
    font-size: var(--text-sm);
  }
  .rows {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
  }
  .item {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
  }
  .command {
    display: grid;
    gap: 6px;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--sunken);
    min-width: 0;
  }
  .cell {
    display: grid;
    grid-template-columns: 76px minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
  }
  .cell-label {
    font-size: var(--text-xs);
    font-weight: 560;
    color: var(--ink-2);
  }
  .command-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .check input {
    margin: 0;
    accent-color: var(--ink-2);
  }
  .remove {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border: 1px solid transparent;
    border-radius: var(--radius);
    background: none;
    color: var(--muted);
    cursor: pointer;
  }
  .remove:hover {
    background: var(--danger-soft);
    color: var(--danger);
  }
  @media (max-width: 520px) {
    .cell {
      grid-template-columns: minmax(0, 1fr);
      gap: 4px;
    }
  }
</style>

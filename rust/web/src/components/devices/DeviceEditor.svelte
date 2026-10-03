<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import Toggle from '../Toggle.svelte';
  import CommandList from './CommandList.svelte';
  import { api, PERM_ALL, type Client, type DisplayDevice } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import {
    changes,
    PERMISSION_GROUPS,
    toDraft,
    VIEW_ONLY,
    VIRTUAL_DISPLAY_LAYOUTS,
    VIRTUAL_DISPLAY_MODES,
    validDisplayMode,
    type Draft,
  } from './device';

  let {
    client,
    dirty = $bindable(false),
    onsaved,
  }: { client: Client; dirty?: boolean; onsaved?: () => void } = $props();

  const uid = $props.id();

  // `saved` is what the host has; `draft` is what the form shows.
  let saved = $state<Draft>(untrack(() => toDraft(client)));
  let draft = $state<Draft>(untrack(() => toDraft(client)));
  let saving = $state(false);

  const pending = $derived(changes(saved, draft));
  const changed = $derived(Object.keys(pending).length > 0);
  const nameMissing = $derived(draft.name.trim() === '');
  const modeInvalid = $derived(!validDisplayMode(draft.display_mode));

  $effect(() => {
    dirty = changed;
  });

  // Follow edits made elsewhere while this form has none of its own.
  $effect(() => {
    const latest = toDraft(client);
    untrack(() => {
      if (!changed && Object.keys(changes(saved, latest)).length > 0) {
        saved = latest;
        draft = toDraft(client);
      }
    });
  });

  let displays = $state<DisplayDevice[] | null>(null);
  let displaysError = $state('');
  let profiles = $state<string[] | null>(null);
  let profilesError = $state('');

  const reason = (error: unknown) => (error instanceof Error ? error.message : String(error));

  onMount(() => {
    api.displays
      .devices()
      .then((list) => (displays = list))
      .catch((error: unknown) => {
        displays = [];
        displaysError = `Could not list displays: ${reason(error)}`;
      });
    api.clients
      .hdrProfiles()
      .then((result) => (profiles = result.profiles.map((profile) => profile.filename)))
      .catch((error: unknown) => {
        profiles = [];
        profilesError = `Could not list color profiles: ${reason(error)}`;
      });
  });

  const displayOptions = $derived.by(() => {
    const current = draft.output_name_override;
    const options: { value: string; label: string }[] = [];
    for (const display of displays ?? []) {
      // The host matches the override against the device id or the display name.
      const isCurrent =
        current !== '' &&
        [display.device_id, display.display_name].some((value) => value.toLowerCase() === current.toLowerCase());
      const value = isCurrent ? current : display.device_id || display.display_name;
      if (!value || options.some((option) => option.value === value)) continue;
      const name = display.friendly_name ? `${display.friendly_name} (${display.display_name})` : display.display_name;
      options.push({ value, label: display.primary ? `${name}, primary` : name });
    }
    if (current && !options.some((option) => option.value === current)) {
      options.unshift({ value: current, label: displays ? `${current} (not connected)` : current });
    }
    return options;
  });

  const profileOptions = $derived.by(() => {
    const current = draft.hdr_profile;
    const options = (profiles ?? []).map((filename) => ({ value: filename, label: filename }));
    if (current && !profiles?.includes(current)) {
      options.unshift({ value: current, label: profiles ? `${current} (not found)` : current });
    }
    return options;
  });

  const layoutHint = $derived(
    VIRTUAL_DISPLAY_LAYOUTS.find((layout) => layout.value === draft.virtual_display_layout)?.hint ??
      'How the virtual display sits beside the monitors.',
  );

  function setPermission(bit: number, granted: boolean) {
    draft.perm = granted ? draft.perm | bit : draft.perm & ~bit;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!changed || nameMissing || modeInvalid) return;
    saving = true;
    // Edits made while the request is out stay unsaved.
    const submitted = $state.snapshot(draft);
    try {
      // null clears a per-device setting, which the Client type does not spell out.
      await api.clients.update({ uuid: client.uuid, ...pending } as Partial<Client> & { uuid: string });
      saved = submitted;
      notify(`Saved ${submitted.name.trim()}.`, 'ok');
      onsaved?.();
    } catch (error) {
      failed('Saving the device failed', error);
    } finally {
      saving = false;
    }
  }

  function discard() {
    draft = $state.snapshot(saved);
  }
</script>

<form class="editor" onsubmit={save} aria-label="Edit {client.name}">
  <section>
    <h3>General</h3>
    <div class="form-grid">
      <Field label="Name" id="{uid}-name" error={nameMissing ? 'Enter a name.' : undefined}>
        <input id="{uid}-name" class="input" autocomplete="off" required bind:value={draft.name} />
      </Field>
      <div class="toggle-cell">
        <Toggle
          label="Enabled"
          hint="When off, this device cannot connect. Saving with it off disconnects the device."
          bind:checked={draft.enabled}
        />
      </div>
    </div>
  </section>

  <section>
    <div class="section-head">
      <h3>Permissions</h3>
      <div class="row">
        <span class="muted presets">Set to</span>
        <Button size="sm" onclick={() => (draft.perm = PERM_ALL)}>Full control</Button>
        <Button size="sm" onclick={() => (draft.perm = VIEW_ONLY)}>View only</Button>
      </div>
    </div>
    <p class="note">
      The first paired device gets everything. Later ones start with List apps and View streams.
    </p>
    <div class="groups">
      {#each PERMISSION_GROUPS as group (group.name)}
        <fieldset>
          <legend>{group.name}</legend>
          {#each group.items as item (item.bit)}
            <label class="check">
              <input
                type="checkbox"
                checked={(draft.perm & item.bit) === item.bit}
                onchange={(event) => setPermission(item.bit, event.currentTarget.checked)}
              />
              {item.label}
            </label>
          {/each}
        </fieldset>
      {/each}
    </div>
  </section>

  <section>
    <h3>Device preferences</h3>
    <p class="note">"Use the host setting" follows the value in Settings.</p>
    <div class="form-grid">
      <Field
        label="Display"
        id="{uid}-display"
        hint="The monitor to stream when this device does not use a virtual display."
        error={displaysError || undefined}
      >
        <select id="{uid}-display" class="select" bind:value={draft.output_name_override}>
          <option value="">Use the host setting</option>
          {#if displays === null}
            <option disabled value="-">Loading displays…</option>
          {/if}
          {#each displayOptions as option (option.value)}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </Field>
      <Field
        label="Display mode"
        id="{uid}-display-mode"
        hint="Width, height and refresh for this device's display, such as 2560x1600x120. Empty uses the mode the device asks for. The stream keeps the device's frame rate."
        error={modeInvalid ? 'Use WIDTHxHEIGHTxREFRESH, such as 1920x1080x59.94.' : undefined}
      >
        <input
          id="{uid}-display-mode"
          class="input mono"
          autocomplete="off"
          spellcheck="false"
          placeholder="The device's own"
          bind:value={draft.display_mode}
        />
      </Field>
      <div class="toggle-cell">
        <Toggle
          label="Always use a virtual display"
          hint="Creates a virtual display for this device even when the host streams a monitor."
          bind:checked={draft.always_use_virtual_display}
        />
      </div>
      <Field
        label="Virtual display mode"
        id="{uid}-mode"
        hint="Whether this device gets its own virtual display or shares one."
      >
        <select id="{uid}-mode" class="select" bind:value={draft.virtual_display_mode}>
          <option value="">Use the host setting</option>
          {#each VIRTUAL_DISPLAY_MODES as mode (mode.value)}
            <option value={mode.value}>{mode.label}</option>
          {/each}
        </select>
      </Field>
      <Field label="Virtual display layout" id="{uid}-layout" hint={layoutHint}>
        <select id="{uid}-layout" class="select" bind:value={draft.virtual_display_layout}>
          <option value="">Use the host setting</option>
          {#each VIRTUAL_DISPLAY_LAYOUTS as layout (layout.value)}
            <option value={layout.value}>{layout.label}</option>
          {/each}
        </select>
      </Field>
      <Field
        label="Prefer 10-bit SDR"
        id="{uid}-sdr"
        hint="When on, HDR requests from this device stream as 10-bit SDR and the display stays in SDR. Needs HEVC or AV1."
      >
        <select id="{uid}-sdr" class="select" bind:value={draft.prefer_10bit_sdr}>
          <option value="">Use the host setting</option>
          <option value="on">On</option>
          <option value="off">Off</option>
        </select>
      </Field>
      <Field
        label="HDR color profile"
        id="{uid}-profile"
        hint="Applied to the display while this device streams in HDR."
        error={profilesError || undefined}
      >
        <select id="{uid}-profile" class="select" bind:value={draft.hdr_profile}>
          <option value="">None</option>
          {#if profiles === null}
            <option disabled value="-">Loading profiles…</option>
          {/if}
          {#each profileOptions as option (option.value)}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </Field>
    </div>
  </section>

  <section>
    <h3>Commands</h3>
    <Toggle
      label="Run connect and disconnect commands"
      hint="An app can still turn these off for its own launches."
      bind:checked={draft.allow_client_commands}
    />
    <div class="command-lists">
      <CommandList label="Connect" hint="Run on this PC when the device starts streaming." bind:rows={draft.do} />
      <CommandList label="Disconnect" hint="Run on this PC when its stream ends." bind:rows={draft.undo} />
    </div>
    <p class="note">
      Commands can use <code>$(SUNSHINE_CLIENT_NAME)</code>, <code>$(SUNSHINE_CLIENT_WIDTH)</code>,
      <code>$(SUNSHINE_CLIENT_HEIGHT)</code>, <code>$(SUNSHINE_CLIENT_FPS)</code> and <code>$(SUNSHINE_CLIENT_HDR)</code>.
    </p>
  </section>

  <footer>
    <Button type="submit" variant="primary" busy={saving} disabled={!changed || nameMissing || modeInvalid}>Save</Button>
    <Button disabled={!changed || saving} onclick={discard}>Discard</Button>
    <span class="status muted">
      {#if client.connected}
        Saving ends this device's current stream.
      {:else if changed}
        Unsaved changes
      {/if}
    </span>
  </footer>
</form>

<style>
  .editor {
    display: grid;
    border-top: 1px solid var(--line);
    background: var(--sunken);
  }
  section {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
    border-bottom: 1px solid var(--line);
    min-width: 0;
  }
  h3 {
    font-size: var(--text-sm);
  }
  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-2) var(--space-4);
    flex-wrap: wrap;
  }
  .presets {
    font-size: var(--text-sm);
    margin-right: var(--space-1);
  }
  .note {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
  }
  .note code {
    color: var(--ink-2);
  }
  .toggle-cell {
    align-self: center;
  }
  .groups {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: var(--space-3);
  }
  fieldset {
    display: grid;
    gap: 6px;
    align-content: start;
    margin: 0;
    padding: var(--space-2) var(--space-3) var(--space-3);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--panel);
    min-width: 0;
  }
  legend {
    padding: 0 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .check input {
    margin: 0;
    accent-color: var(--ink-2);
  }
  .command-lists {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-4) var(--space-5);
  }
  footer {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    padding: var(--space-3) var(--space-5);
    background: var(--panel);
    /* Sits on the last section's rule; stays visible when the footer sticks. */
    box-shadow: 0 -1px 0 var(--line);
  }
  .status {
    font-size: var(--text-xs);
    margin-left: var(--space-2);
  }
  @media (max-width: 900px) {
    .command-lists {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  @media (max-width: 520px) {
    section,
    footer {
      padding-left: var(--space-4);
      padding-right: var(--space-4);
    }
  }
</style>

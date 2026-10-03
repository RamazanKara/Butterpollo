<script lang="ts">
  import { untrack } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Field from '../Field.svelte';
  import Icon from '../Icon.svelte';
  import PageHeader from '../PageHeader.svelte';
  import Panel from '../Panel.svelte';
  import Toggle from '../Toggle.svelte';
  import CoverPicker from './CoverPicker.svelte';
  import DetachedList from './DetachedList.svelte';
  import DisplaySettings from './DisplaySettings.svelte';
  import NumberField from './NumberField.svelte';
  import PrepCommands from './PrepCommands.svelte';
  import RtxSettings from './RtxSettings.svelte';
  import VideoSettings from './VideoSettings.svelte';
  import { api, type App, type SessionStatus } from '../../lib/api';
  import { confirm, failed, notify } from '../../lib/feedback.svelte';
  import { link, navigate } from '../../lib/router.svelte';
  import { closeApp, confirmClose, launchApp, runningName } from './app';
  import { AppDraft, type FlagKey } from './draft.svelte';

  let {
    app,
    position,
    status,
    onsaved,
    onmove,
    ondeleted,
    onstatus,
  }: {
    /** Missing for a new app. */
    app?: App;
    /** Where the app sits in the library; null when it cannot move. */
    position: { index: number; count: number } | null;
    status: SessionStatus | null;
    onsaved: (uuid: string, created: boolean) => Promise<void>;
    onmove: (uuid: string, delta: -1 | 1) => Promise<void>;
    ondeleted: (uuid: string) => void;
    onstatus: () => void;
  } = $props();

  const uid = $props.id();
  const draft = new AppDraft(untrack(() => app));

  let saving = $state(false);
  /** 'launch', 'close', 'delete' or 'move' while a request is out. */
  let busy = $state('');

  const uuid = $derived(draft.app.uuid);
  const isNew = $derived(!draft.saved.uuid);
  const savedName = $derived(draft.saved.name.trim());
  const running = $derived(runningName(status));
  const isRunning = $derived(!isNew && running !== '' && running === draft.saved.name);
  const others = $derived(draft.others());

  // Follow a reload of the library while this form has no edits of its own.
  $effect(() => {
    const latest = app;
    untrack(() => {
      if (latest && !saving && !draft.changed && !draft.matches(latest)) draft.reset(latest);
    });
  });

  // Unsaved edits: warn before the tab closes and before any link leaves the editor.
  $effect(() => {
    if (!draft.changed) return;
    const beforeUnload = (event: BeforeUnloadEvent) => event.preventDefault();
    const click = (event: MouseEvent) => {
      if (event.defaultPrevented || event.button !== 0) return;
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null;
      if (!(anchor instanceof HTMLAnchorElement) || anchor.target || anchor.hasAttribute('download')) return;
      const url = new URL(anchor.href, location.href);
      if (url.origin !== location.origin || url.pathname.startsWith('/api/')) return;
      const to = url.pathname + url.search;
      if (to === location.pathname + location.search) return;
      // Runs before the router's own link handler, which skips prevented clicks.
      event.preventDefault();
      void leave(to);
    };
    window.addEventListener('beforeunload', beforeUnload);
    document.addEventListener('click', click, true);
    return () => {
      window.removeEventListener('beforeunload', beforeUnload);
      document.removeEventListener('click', click, true);
    };
  });

  async function leave(to: string) {
    const ok = await confirm({
      title: 'Discard changes?',
      message: `The changes you made to ${savedName || 'this app'} are not saved.`,
      confirm: 'Discard',
    });
    if (ok) {
      draft.reset(draft.saved);
      navigate(to);
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!draft.changed || draft.invalid || saving) return;
    saving = true;
    const record = draft.record();
    const created = !record.uuid;
    try {
      const result = await api.apps.save(record);
      const id = record.uuid ?? result.uuid;
      draft.markSaved({ ...record, uuid: id });
      notify(`Saved ${record.name.trim()}.`, 'ok');
      await onsaved(id, created);
    } catch (error) {
      failed('Saving the app failed', error);
    } finally {
      saving = false;
    }
  }

  function discard() {
    draft.reset(draft.saved);
  }

  async function launch() {
    busy = 'launch';
    await launchApp(draft.saved);
    busy = '';
    onstatus();
  }

  async function close() {
    if (!(await confirmClose(running))) return;
    busy = 'close';
    await closeApp(running);
    busy = '';
    onstatus();
  }

  async function remove() {
    const id = draft.saved.uuid;
    if (!id) return;
    const name = savedName || 'this app';
    const ok = await confirm({
      title: `Delete ${name}?`,
      message: 'It is removed from the library and devices can no longer start it. This can’t be undone.',
      confirm: 'Delete',
      danger: true,
    });
    if (!ok) return;
    busy = 'delete';
    try {
      await api.apps.remove(id);
      notify(`Deleted ${name}.`, 'ok');
      draft.reset(draft.saved);
      ondeleted(id);
    } catch (error) {
      failed('Deleting the app failed', error);
    } finally {
      busy = '';
    }
  }

  async function move(delta: -1 | 1) {
    const id = draft.saved.uuid;
    if (!id) return;
    busy = 'move';
    await onmove(id, delta);
    busy = '';
  }
</script>

{#snippet flag(key: FlagKey, label: string, hint?: string)}
  <Toggle {label} {hint} bind:checked={() => draft.flag(key), (value) => draft.setFlag(key, value)} />
{/snippet}

<div class="editor">
  <a class="back" href="/library" use:link>
    <span class="back-icon"><Icon name="chevron" size={16} /></span>
    Library
  </a>

  <PageHeader
    title={isNew ? 'New app' : savedName || 'Unnamed app'}
    subtitle={isNew ? 'Give it a name and the command to run, then save.' : undefined}
  >
    {#snippet actions()}
      <div class="header-actions">
        {#if isRunning}
          {#if status?.paused}<Badge tone="warn">Paused</Badge>{:else}<Badge tone="live">Running</Badge>{/if}
        {/if}
        {#if position}
          <span class="muted position num">{position.index + 1} of {position.count}</span>
          <Button size="sm" disabled={position.index === 0 || busy === 'move'} onclick={() => move(-1)}>
            Move earlier
          </Button>
          <Button
            size="sm"
            disabled={position.index >= position.count - 1 || busy === 'move'}
            onclick={() => move(1)}
          >
            Move later
          </Button>
        {/if}
      </div>
    {/snippet}
  </PageHeader>

  <form class="sections" onsubmit={save} aria-label={isNew ? 'New app' : `Edit ${savedName}`}>
    <div class="top">
      <Panel title="General">
        <div class="fields">
          <Field
            label="Name"
            id="{uid}-name"
            error={draft.nameMissing && draft.changed ? 'Enter a name.' : undefined}
          >
            <input id="{uid}-name" class="input" autocomplete="off" required bind:value={draft.app.name} />
          </Field>
          <Field label="Command" id="{uid}-cmd" hint="Empty streams the desktop.">
            <input
              id="{uid}-cmd"
              class="input mono"
              autocomplete="off"
              spellcheck="false"
              bind:value={draft.app.cmd}
            />
          </Field>
          <Field label="Working directory" id="{uid}-dir" hint="Empty uses the program’s folder.">
            <input
              id="{uid}-dir"
              class="input mono"
              autocomplete="off"
              spellcheck="false"
              bind:value={draft.app['working-dir']}
            />
          </Field>
          <DetachedList id="{uid}-detached" bind:rows={draft.detached} />
          {#if uuid}
            <p class="id muted">ID <span class="mono">{uuid}</span></p>
          {/if}
        </div>
      </Panel>

      <Panel title="Cover">
        <CoverPicker {draft} />
      </Panel>
    </div>

    <Panel title="Preparation commands" description="Run before the app starts. Undo commands run after it closes.">
      <div class="prep">
        <PrepCommands id="{uid}-prep" bind:rows={draft.prep} />
        <p class="note">
          Commands can use <code>$(SUNSHINE_APP_NAME)</code>, <code>$(SUNSHINE_CLIENT_WIDTH)</code>,
          <code>$(SUNSHINE_CLIENT_HEIGHT)</code>, <code>$(SUNSHINE_CLIENT_FPS)</code> and
          <code>$(SUNSHINE_CLIENT_HDR)</code>.
        </p>
        <Toggle
          label="Skip the global preparation commands"
          hint="The ones set in Settings."
          bind:checked={() => draft.flag('exclude-global-prep-cmd'),
          (value) => draft.setFlag('exclude-global-prep-cmd', value)}
        />
      </div>
    </Panel>

    <Panel title="Behaviour" description="What the host does while the app runs.">
      <div class="toggles">
        {@render flag('elevated', 'Run as administrator', 'Detached commands run as administrator too.')}
        {@render flag(
          'auto-detach',
          'Keep the stream when a launcher exits within 5 seconds',
          'For launchers that start the game and quit.',
        )}
        {@render flag(
          'wait-all',
          'Wait for every process the app starts',
          'The app counts as running until the last one exits.',
        )}
        {@render flag('terminate-on-pause', 'Close the app when the last device disconnects')}
        {@render flag(
          'allow-client-commands',
          'Run devices’ connect and disconnect commands',
          'Set for each device under Devices.',
        )}
        {@render flag(
          'exclude-global-state-cmd',
          'Skip the global pause and resume commands',
          'The ones set in Settings.',
        )}
      </div>
      <div class="form-grid behaviour-numbers">
        <NumberField
          {draft}
          key="exit-timeout"
          label="Exit timeout"
          hint="How long to wait for the app to close before ending it. 0 to 300, default 10."
          placeholder="10"
          unit="s"
        />
      </div>
    </Panel>

    <Panel title="Display">
      <DisplaySettings {draft} />
    </Panel>

    <Panel title="Video and input">
      <VideoSettings {draft} />
    </Panel>

    <Panel title="NVIDIA RTX HDR">
      <RtxSettings {draft} running={isRunning} />
    </Panel>

    {#if others.length}
      <details class="others">
        <summary>Other settings <span class="muted num">({others.length})</span></summary>
        <p class="muted note">This page has no controls for these. They are kept as they are when you save.</p>
        <dl class="facts">
          {#each others as [key, value] (key)}
            <dt class="mono">{key}</dt>
            <dd class="mono">{JSON.stringify(value)}</dd>
          {/each}
        </dl>
      </details>
    {/if}

    <footer class="bar">
      <Button type="submit" variant="primary" busy={saving} disabled={!draft.changed || draft.invalid}>Save</Button>
      <Button disabled={!draft.changed || saving} onclick={discard}>Discard</Button>
      <span class="status muted" aria-live="polite">{draft.changed ? 'Unsaved changes' : ''}</span>
      {#if !isNew}
        <span class="end">
          {#if isRunning}
            <Button icon="stop" busy={busy === 'close'} onclick={close}>Close app</Button>
          {:else}
            <Button
              icon="play"
              busy={busy === 'launch'}
              disabled={running !== '' || busy !== ''}
              title={running
                ? `Close ${running} first`
                : draft.changed
                  ? 'Starts it with the saved settings'
                  : undefined}
              onclick={launch}
            >
              Launch
            </Button>
          {/if}
          <Button variant="danger" icon="trash" busy={busy === 'delete'} disabled={saving} onclick={remove}>
            Delete
          </Button>
        </span>
      {/if}
    </footer>
  </form>
</div>

<style>
  .editor {
    display: grid;
    gap: var(--space-4);
    min-width: 0;
  }
  .editor :global(.page-header) {
    margin-bottom: 0;
  }
  .back {
    justify-self: start;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-sm);
    color: var(--ink-2);
    text-decoration: none;
  }
  .back:hover {
    color: var(--ink);
    text-decoration: underline;
  }
  .back-icon {
    display: inline-flex;
    transform: rotate(180deg);
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  .position {
    font-size: var(--text-xs);
    margin-right: 2px;
  }
  .sections {
    display: grid;
    gap: var(--space-4);
    min-width: 0;
  }
  .top {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(220px, 300px);
    gap: var(--space-4);
    align-items: start;
  }
  .fields {
    display: grid;
    gap: var(--space-4);
  }
  .id {
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .toggles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: var(--space-4) var(--space-5);
  }
  .behaviour-numbers {
    margin-top: var(--space-5);
  }
  .prep {
    display: grid;
    gap: var(--space-4);
  }
  .note {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
  }
  .note code {
    color: var(--ink-2);
  }
  .others {
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    background: var(--panel);
    padding: var(--space-3) var(--space-5);
  }
  .others summary {
    cursor: pointer;
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .others[open] summary {
    margin-bottom: var(--space-3);
  }
  .others .note {
    margin-bottom: var(--space-3);
  }
  .others dt,
  .others dd {
    font-size: var(--text-xs);
  }
  .bar {
    position: sticky;
    bottom: var(--space-3);
    z-index: 5;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line);
    border-radius: var(--radius-lg);
    background: var(--panel);
    box-shadow: var(--shadow-pop);
  }
  .status {
    font-size: var(--text-xs);
    margin-left: var(--space-1);
  }
  .end {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
    margin-left: auto;
  }
  @media (max-width: 900px) {
    .top {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  @media (max-width: 520px) {
    .end {
      margin-left: 0;
    }
  }
</style>

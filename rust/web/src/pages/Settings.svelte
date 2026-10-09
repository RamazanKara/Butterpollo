<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../components/Button.svelte';
  import EmptyState from '../components/EmptyState.svelte';
  import Icon from '../components/Icon.svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import Panel from '../components/Panel.svelte';
  import SettingRow from '../components/settings/SettingRow.svelte';
  import UnknownKeys from '../components/settings/UnknownKeys.svelte';
  import {
    applyChanges,
    computeChanges,
    currentValues,
    effective,
    has,
    normalise,
    NOT_SETTINGS,
    sameValue,
    settingsOnly,
    type Edits,
  } from '../components/settings/values';
  import { api, type Config, type ConfigValue } from '../lib/api';
  import { confirm, failed, notify } from '../lib/feedback.svelte';
  import { guardNavigation, match, navigate } from '../lib/router.svelte';
  import { refreshMetadata } from '../lib/session.svelte';
  import { schema } from '../lib/settings-schema';
  import type { Category, CategoryId, Setting } from '../lib/settings-types';

  const categories = schema.categories;
  const byKey: ReadonlyMap<string, Setting> = new Map(schema.settings.map((setting) => [setting.key, setting]));
  const isDescribed = (key: string) => byKey.has(key);

  const section = $derived(match('/settings/:section')?.section ?? 'general');
  const active = $derived<Category | undefined>(
    categories.find((category) => category.id === section) ??
      categories.find((category) => category.id === 'general') ??
      categories[0],
  );

  /** What the host has stored; null until loaded. */
  let stored = $state<Config | null>(null);
  let loadError = $state('');
  let edits = $state<Edits>({});
  /** JSON text that does not parse, by key. Saving waits until it does. */
  let invalid = $state<Record<string, string>>({});
  let query = $state('');
  let saving = $state(false);
  let saved = $state(false);
  let restarting = $state(false);
  let latestLoad = 0;

  const config = $derived(stored ?? {});
  const values = $derived(currentValues(schema.settings, edits, config));
  const changes = $derived(stored ? computeChanges(byKey, edits, stored) : {});
  const pending = $derived(new Set([...Object.keys(changes), ...Object.keys(invalid)]));
  const invalidCount = $derived(Object.keys(invalid).length);

  const changedIn = $derived.by(() => {
    const counts: Partial<Record<CategoryId, number>> = {};
    for (const key of pending) {
      const id = byKey.get(key)?.category ?? 'advanced';
      counts[id] = (counts[id] ?? 0) + 1;
    }
    return counts;
  });

  /** Stored or newly added keys the schema does not describe. */
  const unknownKeys = $derived(
    [...new Set([...Object.keys(config), ...Object.keys(edits)])]
      .filter((key) => !byKey.has(key) && !NOT_SETTINGS.has(key))
      .sort(),
  );

  function visible(setting: Setting): boolean {
    if (!setting.visibleWhen) return true;
    try {
      return setting.visibleWhen(values);
    } catch {
      return true;
    }
  }

  interface Group {
    name: string;
    settings: Setting[];
  }

  /** Settings under their group headings, groups in order of first use. */
  function grouped(list: Setting[]): Group[] {
    const groups: Group[] = [];
    for (const setting of list) {
      const name = setting.group ?? '';
      let group = groups.find((candidate) => candidate.name === name);
      if (!group) {
        group = { name, settings: [] };
        groups.push(group);
      }
      group.settings.push(setting);
    }
    return groups;
  }

  const inCategory = $derived(schema.settings.filter((setting) => setting.category === active?.id));
  const shown = $derived(inCategory.filter(visible));
  const basic = $derived(grouped(shown.filter((setting) => !setting.advanced)));
  const advanced = $derived(shown.filter((setting) => setting.advanced));
  const advancedChanged = $derived(advanced.filter((setting) => pending.has(setting.key)).length);

  // ---------- search ----------

  const words = $derived(query.trim().toLowerCase().split(/\s+/).filter(Boolean));
  const searching = $derived(words.length > 0);
  const hits = (text: string) => words.every((word) => text.includes(word));

  const results = $derived.by(() => {
    if (!searching) return [];
    return categories
      .map((category) => ({
        category,
        settings: schema.settings.filter(
          (setting) =>
            setting.category === category.id &&
            hits(`${setting.label}\n${setting.key}\n${setting.description}`.toLowerCase()),
        ),
        unknown: category.id === 'advanced' ? unknownKeys.filter((key) => hits(key.toLowerCase())) : [],
      }))
      .filter((result) => result.settings.length > 0 || result.unknown.length > 0);
  });

  // ---------- loading and editing ----------

  async function load() {
    // A load that started before a save must not put the old values back.
    const id = ++latestLoad;
    try {
      const config = settingsOnly(await api.config.get());
      if (id !== latestLoad) return;
      stored = config;
      loadError = '';
    } catch (error) {
      if (id === latestLoad) loadError = error instanceof Error ? error.message : String(error);
    }
  }

  onMount(() => {
    void load();
    // Fresh display, GPU and audio lists for the pickers.
    refreshMetadata().catch(() => {});
  });

  function edit(key: string, value: ConfigValue) {
    edits[key] = value;
  }
  function reset(key: string) {
    edits[key] = null;
    delete invalid[key];
  }
  function undo(key: string) {
    delete edits[key];
    delete invalid[key];
  }
  function setInvalid(key: string, text: string | null) {
    if (text === null) delete invalid[key];
    else invalid[key] = text;
  }
  function removeUnknown(key: string) {
    if (has(config, key)) edits[key] = null;
    else delete edits[key];
  }
  function discard() {
    edits = {};
    invalid = {};
  }

  function open(id: string, event?: MouseEvent) {
    if (event) {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
    }
    query = '';
    navigate(`/settings/${id}`);
  }

  async function save() {
    const base = stored;
    if (!base || saving || pending.size === 0 || invalidCount > 0) return;
    const body = $state.snapshot(changes) as Config;
    // Edits made while the request is out stay unsaved.
    const submitted = $state.snapshot(edits) as Edits;
    saving = true;
    latestLoad++;
    try {
      const result = await api.config.patch(body);
      stored = applyChanges(stored ?? base, body);
      for (const [key, value] of Object.entries(submitted)) {
        if (has(edits, key) && sameValue(edits[key], value)) delete edits[key];
      }
      saved = result.restart_required !== false;
      if (!saved) notify('Settings saved.', 'ok');
      if (result.warning) notify(result.warning, 'warn', 8000);
      void load();
    } catch (error) {
      failed('Saving settings failed', error);
    } finally {
      saving = false;
    }
  }

  async function restart() {
    const ok = await confirm({
      title: 'Restart Rubylight?',
      message: 'Streams in progress disconnect. This console reconnects by itself when the host is back.',
      confirm: 'Restart',
    });
    if (!ok) return;
    restarting = true;
    try {
      await api.host.restart();
      saved = false;
      notify('Rubylight is restarting. This console reconnects when the host is back.', 'info', 8000);
    } catch (error) {
      failed('Restart failed', error);
    } finally {
      restarting = false;
    }
  }

  // Leaving the page with unsaved changes asks first; switching categories
  // keeps the edits.
  $effect(() => {
    if (pending.size === 0) return;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener('beforeunload', warn);
    const unguard = guardNavigation((to) =>
      to.startsWith('/settings')
        ? true
        : confirm({
            title: 'Leave without saving?',
            message: `${pending.size} unsaved ${pending.size === 1 ? 'change is' : 'changes are'} lost.`,
            confirm: 'Leave',
            danger: true,
          }),
    );
    return () => {
      window.removeEventListener('beforeunload', warn);
      unguard();
    };
  });
</script>

{#snippet row(setting: Setting, inactive: boolean)}
  <SettingRow
    {setting}
    value={normalise(setting, effective(setting, edits, config))}
    stored={has(config, setting.key)}
    changed={has(changes, setting.key)}
    resetting={edits[setting.key] === null && has(config, setting.key)}
    {inactive}
    invalidText={invalid[setting.key]}
    onchange={(value) => edit(setting.key, value)}
    onreset={() => reset(setting.key)}
    onundo={() => undo(setting.key)}
    oninvalid={(text) => setInvalid(setting.key, text)}
  />
{/snippet}

{#snippet groups(list: Group[])}
  {#each list as group (group.name)}
    <section class="group">
      {#if group.name}<h3 class="group-title">{group.name}</h3>{/if}
      <!-- Keyed by object: a key may be listed in two categories. -->
      {#each group.settings as setting (setting)}
        {@render row(setting, false)}
      {/each}
    </section>
  {/each}
{/snippet}

{#snippet otherKeys(keys: string[], adding: boolean)}
  <UnknownKeys
    {keys}
    stored={config}
    {edits}
    {changes}
    {isDescribed}
    {adding}
    onedit={edit}
    onremove={removeUnknown}
    onundo={undo}
  />
{/snippet}

<PageHeader title="Settings" subtitle="How this host captures, encodes and streams. Most changes apply after the host restarts." />

<div class="frame">
  <div class="layout">
    <div class="side">
      <div class="search">
        <Icon name="search" size={16} />
        <input
          class="input"
          type="search"
          placeholder="Search settings"
          aria-label="Search settings"
          autocomplete="off"
          spellcheck="false"
          bind:value={query}
        />
      </div>

      <label class="picker">
        <span>Category</span>
        <select class="select" onchange={(event) => open(event.currentTarget.value)}>
          {#each categories as category (category.id)}
            {@const count = changedIn[category.id] ?? 0}
            <option value={category.id} selected={category.id === active?.id}>
              {category.label}{count ? ` (${count} unsaved)` : ''}
            </option>
          {/each}
        </select>
      </label>

      <nav aria-label="Settings categories">
        <ul>
          {#each categories as category (category.id)}
            {@const count = changedIn[category.id] ?? 0}
            <li>
              <a
                href="/settings/{category.id}"
                aria-current={!searching && category.id === active?.id ? 'page' : undefined}
                onclick={(event) => open(category.id, event)}
              >
                <span class="name">
                  {category.label}
                  {#if count}
                    <span class="dot" aria-hidden="true"></span>
                    <span class="visually-hidden">, {count} unsaved</span>
                  {/if}
                </span>
                <span class="about">{category.description}</span>
              </a>
            </li>
          {/each}
        </ul>
      </nav>
    </div>

    <div class="content">
      {#if loadError}
        <div class="notice danger load-error" role="alert">
          <span>{stored ? 'Could not refresh the settings' : 'Could not load the settings'}: {loadError}</span>
          <Button size="sm" onclick={load}>Retry</Button>
        </div>
      {/if}

      {#if stored === null}
        {#if !loadError}<p class="placeholder muted">Loading settings…</p>{/if}
      {:else if searching}
        {#if results.length === 0}
          <Panel>
            <EmptyState icon="search" title="No settings match">
              <p>Search looks at names, keys and descriptions.</p>
            </EmptyState>
          </Panel>
        {:else}
          {#each results as result (result.category.id)}
            <Panel title={result.category.label} flush>
              {#snippet actions()}
                <Button size="sm" variant="ghost" onclick={() => open(result.category.id)}>
                  Open<span class="visually-hidden">{` ${result.category.label}`}</span>
                </Button>
              {/snippet}
              <div class="rows">
                {#each result.settings as setting (setting)}
                  {@render row(setting, !visible(setting))}
                {/each}
              </div>
              {#if result.unknown.length}
                {@render otherKeys(result.unknown, false)}
              {/if}
            </Panel>
          {/each}
        {/if}
      {:else if active}
        <Panel title={active.label} description={active.description} flush>
          {#if inCategory.length === 0}
            {#if active.id !== 'advanced'}
              <EmptyState icon="settings" title="No settings in this category">
                <p>There are no {active.label.toLowerCase()} settings available.</p>
              </EmptyState>
            {/if}
          {:else if basic.length === 0}
            <!-- Nothing to fold advanced settings behind. -->
            <div class="rows">
              {@render groups(grouped(advanced))}
              {#if shown.length === 0}
                <p class="placeholder muted">None of these settings apply with the current choices.</p>
              {/if}
            </div>
          {:else}
            <div class="rows">
              {@render groups(basic)}
            </div>
            {#if advanced.length}
              <details class="advanced">
                <summary>
                  <Icon name="chevron" size={15} />
                  Advanced
                  <span class="muted">
                    {advanced.length}
                    {advanced.length === 1 ? 'setting' : 'settings'}{advancedChanged ? `, ${advancedChanged} unsaved` : ''}
                  </span>
                </summary>
                <div class="rows">
                  {@render groups(grouped(advanced))}
                </div>
              </details>
            {/if}
          {/if}
          {#if active.id === 'advanced'}
            {@render otherKeys(unknownKeys, true)}
          {/if}
        </Panel>
      {/if}
    </div>
  </div>
</div>

{#if stored && (pending.size > 0 || saved)}
  <div class="save-bar">
    {#if pending.size > 0}
      <p class="status" role="status">
        <span class="dot" aria-hidden="true"></span>
        {pending.size} unsaved {pending.size === 1 ? 'change' : 'changes'}
        {#if invalidCount}<span class="problem">Correct the highlighted settings before saving.</span>{/if}
      </p>
      <div class="actions">
        <Button disabled={saving} onclick={discard}>Discard</Button>
        <Button variant="primary" busy={saving} disabled={invalidCount > 0} onclick={save}>Save changes</Button>
      </div>
    {:else}
      <p class="status" role="status">Saved. Some changes apply after the host restarts.</p>
      <div class="actions">
        <Button icon="refresh" busy={restarting} onclick={restart}>Restart now</Button>
        <Button variant="ghost" icon="x" title="Dismiss" onclick={() => (saved = false)}>
          <span class="visually-hidden">Dismiss</span>
        </Button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .frame {
    /* The layout follows the room the page has, whatever the shell around it. */
    container-type: inline-size;
    min-width: 0;
  }
  .layout {
    display: grid;
    grid-template-columns: 236px minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .side {
    position: sticky;
    top: var(--space-4);
    display: grid;
    gap: var(--space-3);
    min-width: 0;
  }
  .search {
    position: relative;
    color: var(--muted);
  }
  .search :global(svg) {
    position: absolute;
    left: 10px;
    top: 50%;
    transform: translateY(-50%);
    pointer-events: none;
  }
  .search .input {
    padding-left: 32px;
  }
  .picker {
    display: none;
    gap: 6px;
    font-size: var(--text-sm);
    font-weight: 560;
  }
  nav ul {
    display: grid;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  nav a {
    display: grid;
    gap: 1px;
    padding: 7px 10px;
    border-radius: var(--radius);
    color: var(--ink-2);
    text-decoration: none;
  }
  nav a:hover {
    background: var(--panel);
    color: var(--ink);
  }
  nav a[aria-current='page'] {
    background: var(--panel);
    color: var(--ink);
    box-shadow:
      inset 3px 0 0 var(--accent),
      0 0 0 1px var(--line);
  }
  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .about {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.4;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
  }
  .content {
    display: grid;
    gap: var(--space-4);
    align-content: start;
    min-width: 0;
  }
  .load-error {
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
  }
  .placeholder {
    padding: var(--space-5);
    font-size: var(--text-sm);
  }
  .rows {
    display: grid;
  }
  .group + .group {
    border-top: 1px solid var(--line);
  }
  .group :global(.setting + .setting) {
    border-top: 1px solid var(--line);
  }
  .rows > :global(.setting + .setting) {
    border-top: 1px solid var(--line);
  }
  .group-title {
    padding: var(--space-2) var(--space-5);
    border-bottom: 1px solid var(--line);
    background: var(--sunken);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--ink-2);
  }
  .advanced {
    border-top: 1px solid var(--line);
  }
  .advanced summary {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    font-size: var(--text-sm);
    font-weight: 560;
    cursor: pointer;
    list-style: none;
  }
  .advanced summary::-webkit-details-marker {
    display: none;
  }
  .advanced summary:hover {
    background: var(--sunken);
  }
  .advanced summary :global(svg) {
    transition: transform 0.12s;
  }
  .advanced[open] summary :global(svg) {
    transform: rotate(90deg);
  }
  .advanced summary .muted {
    font-weight: 400;
    font-size: var(--text-xs);
  }
  .advanced[open] summary {
    border-bottom: 1px solid var(--line);
  }
  .save-bar {
    position: sticky;
    bottom: var(--space-3);
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-4);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-lg);
    background: var(--panel-raised);
    box-shadow: var(--shadow-pop);
  }
  .status {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px var(--space-2);
    font-size: var(--text-sm);
    font-weight: 560;
  }
  .problem {
    color: var(--danger);
    font-weight: 400;
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  @container (max-width: 720px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
      gap: var(--space-4);
    }
    .side {
      position: static;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      align-items: end;
    }
    .picker {
      display: grid;
    }
    nav {
      display: none;
    }
  }
  @media (max-width: 520px) {
    .side {
      grid-template-columns: minmax(0, 1fr);
    }
    .group-title,
    .advanced summary {
      padding-left: var(--space-4);
      padding-right: var(--space-4);
    }
  }
</style>

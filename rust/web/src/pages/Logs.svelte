<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../components/Badge.svelte';
  import Button from '../components/Button.svelte';
  import Icon from '../components/Icon.svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import Toggle from '../components/Toggle.svelte';
  import LogView from '../components/logs/LogView.svelte';
  import { countMatches, parse, passes, searchPattern, type LevelFilter, type LogLine } from '../components/logs/logtext';
  import { api, type LogChunk } from '../lib/api';
  import { failed, notify } from '../lib/feedback.svelte';
  import { poll } from '../lib/format';

  const TAIL_BYTES = 512 * 1024;
  const MAX_LINES = 5000;
  /** A line this long without a newline is shown anyway. */
  const MAX_CARRY = 64 * 1024;

  let lines = $state.raw<LogLine[]>([]);
  let loaded = $state(false);
  let error = $state('');
  let paused = $state(false);
  let follow = $state(true);
  let filter = $state<LevelFilter>('all');
  let query = $state('');
  let current = $state<{ id: number; n: number } | null>(null);

  // Read position in the file, and an unfinished last line.
  let offset = -1;
  let carry = '';

  const visible = $derived(filter === 'all' ? lines : lines.filter((line) => passes(line, filter)));
  const pattern = $derived(searchPattern(query));
  const hits = $derived.by(() => {
    const list: { id: number; first: number; count: number }[] = [];
    let total = 0;
    if (pattern) {
      for (const line of visible) {
        const count = countMatches(line.text, pattern);
        if (count) {
          list.push({ id: line.id, first: total, count });
          total += count;
        }
      }
    }
    return { list, total };
  });
  const matched = $derived(new Set(hits.list.map((hit) => hit.id)));
  /** 1-based position of the current match, or 0. */
  const position = $derived.by(() => {
    const at = current;
    if (!at) return 0;
    const entry = hits.list.find((hit) => hit.id === at.id);
    return entry && at.n < entry.count ? entry.first + at.n + 1 : 0;
  });
  const matchLabel = $derived(
    !pattern
      ? ''
      : hits.total === 0
        ? 'No matches'
        : position
          ? `${position} of ${hits.total}`
          : `${hits.total} ${hits.total === 1 ? 'match' : 'matches'}`,
  );
  const placeholder = $derived(
    !loaded
      ? error
        ? 'The log could not be read.'
        : 'Loading the log…'
      : lines.length === 0
        ? 'The log is empty.'
        : visible.length === 0
          ? filter === 'error'
            ? `No errors in the last ${lines.length} lines.`
            : `No warnings or errors in the last ${lines.length} lines.`
          : '',
  );

  /** The chunk began partway through the file, so its first line is cut. */
  function startsMidFile(chunk: LogChunk) {
    return chunk.offset > new TextEncoder().encode(chunk.text).length;
  }

  async function read() {
    if (paused) return;
    const fresh = offset < 0;
    try {
      const chunk = fresh ? await api.logs.read(-1, TAIL_BYTES) : await api.logs.read(offset);
      let text = chunk.text;
      let base = lines;
      if (fresh || chunk.reset) {
        base = [];
        carry = '';
        if (startsMidFile(chunk)) text = text.slice(text.indexOf('\n') + 1);
      }
      const parts = (carry + text).split('\n');
      carry = parts.pop() ?? '';
      if (carry.length > MAX_CARRY) {
        parts.push(carry);
        carry = '';
      }
      if (parts.length || base !== lines) {
        const next = base.concat(parse(parts, base.at(-1)?.level ?? null));
        lines = next.length > MAX_LINES ? next.slice(-MAX_LINES) : next;
      }
      offset = chunk.offset;
      error = '';
      loaded = true;
    } catch (failure) {
      error = failure instanceof Error ? failure.message : String(failure);
    }
  }

  function go(step: 1 | -1) {
    const total = hits.total;
    if (!total) return;
    const index = position ? (position - 1 + step + total) % total : step > 0 ? 0 : total - 1;
    const entry = hits.list.find((hit) => index >= hit.first && index < hit.first + hit.count);
    if (!entry) return;
    follow = false;
    current = { id: entry.id, n: index - entry.first };
  }

  function searchKeys(event: KeyboardEvent) {
    if (event.key === 'Enter') {
      event.preventDefault();
      go(event.shiftKey ? -1 : 1);
    } else if (event.key === 'Escape') {
      query = '';
      current = null;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(visible.map((line) => line.text).join('\n'));
      notify(`Copied ${visible.length} ${visible.length === 1 ? 'line' : 'lines'}.`, 'ok');
    } catch (failure) {
      failed('Copying failed', failure);
    }
  }

  onMount(() => poll(read, 1000));
</script>

<div class="page">
  <PageHeader title="Logs" subtitle="The host log, read every second. The viewer keeps the latest {MAX_LINES} lines.">
    {#snippet actions()}
      <Button href={api.logs.downloadUrl} icon="download">Download log</Button>
      <Button href={api.logs.supportBundleUrl} icon="download">Download support bundle</Button>
    {/snippet}
  </PageHeader>

  <div class="toolbar">
    <div class="filters">
      <label class="visually-hidden" for="log-level">Level</label>
      <select id="log-level" class="select level" bind:value={filter} onchange={() => (current = null)}>
        <option value="all">All</option>
        <option value="warn">Warnings and errors</option>
        <option value="error">Errors</option>
      </select>
      <div class="find">
        <div class="search">
          <label class="visually-hidden" for="log-search">Search the log</label>
          <span class="search-icon" aria-hidden="true"><Icon name="search" size={15} /></span>
          <input
            id="log-search"
            class="input"
            type="search"
            placeholder="Search"
            autocomplete="off"
            spellcheck="false"
            bind:value={query}
            oninput={() => (current = null)}
            onkeydown={searchKeys}
          />
        </div>
        <span class="count num" aria-live="polite">{matchLabel}</span>
        <div class="steps">
          <button class="step" aria-label="Previous match" title="Previous match (Shift+Enter)" disabled={!hits.total} onclick={() => go(-1)}>
            <span class="up"><Icon name="chevron" size={15} /></span>
          </button>
          <button class="step" aria-label="Next match" title="Next match (Enter)" disabled={!hits.total} onclick={() => go(1)}>
            <span class="down"><Icon name="chevron" size={15} /></span>
          </button>
        </div>
      </div>
    </div>
    <div class="controls">
      {#if paused}
        <Badge>Paused</Badge>
      {:else if error}
        <Badge tone="danger">Not updating</Badge>
      {:else if loaded}
        <Badge tone="live">Live</Badge>
      {/if}
      <Toggle label="Follow" bind:checked={follow} />
      <Button size="sm" onclick={() => (paused = !paused)}>{paused ? 'Resume' : 'Pause'}</Button>
      <Button size="sm" icon="copy" disabled={!visible.length} onclick={copy}>Copy</Button>
    </div>
  </div>

  {#if error}
    <p class="notice danger" role="alert">Could not read the log: {error}. Trying again every second.</p>
  {/if}

  <LogView lines={visible} {pattern} {matched} {current} {placeholder} bind:follow />
</div>

<style>
  .page {
    display: grid;
    gap: var(--space-3);
    min-width: 0;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2) var(--space-4);
  }
  .filters,
  .controls,
  .find {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .filters,
  .controls {
    flex-wrap: wrap;
  }
  .filters {
    flex: 1 1 420px;
  }
  .level {
    width: auto;
    padding-top: 6px;
    padding-bottom: 6px;
    font-size: var(--text-sm);
  }
  .find {
    flex: 1 1 240px;
    max-width: 460px;
  }
  .search {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  .search .input {
    padding: 6px 10px 6px 30px;
    font-size: var(--text-sm);
  }
  .search-icon {
    position: absolute;
    left: 9px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--muted);
    pointer-events: none;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .steps {
    display: flex;
    gap: 2px;
  }
  .step {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius);
    background: var(--panel);
    color: var(--ink-2);
    cursor: pointer;
  }
  .step:hover:not(:disabled) {
    background: var(--sunken);
  }
  .step:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .up {
    transform: rotate(-90deg);
  }
  .down {
    transform: rotate(90deg);
  }
  .controls {
    gap: var(--space-3);
  }
</style>

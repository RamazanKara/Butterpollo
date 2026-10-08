<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import LoadState from './LoadState.svelte';
  import { api, type CrashStatus } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import { bytes, duration } from '../../lib/format';
  import { localTime, reason } from './util';

  let crash = $state<CrashStatus | null>(null);
  let loadError = $state('');
  let busy = $state(false);

  function age(seconds: number) {
    if (seconds < 86400) return `${duration(seconds)} ago`;
    const days = Math.floor(seconds / 86400);
    return `${days} ${days === 1 ? 'day' : 'days'} ago`;
  }
  async function load() {
    try {
      crash = await api.health.crash();
      loadError = '';
    } catch (error) {
      loadError = reason(error);
    }
  }

  async function dismiss() {
    if (!crash?.available) return;
    busy = true;
    try {
      await api.health.dismissCrash(crash.filename, crash.captured_at);
      notify('Crash report dismissed.', 'ok');
      await load();
    } catch (error) {
      failed('Dismissing the crash report failed', error);
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<Panel title="Crash report">
  {#snippet actions()}
    {#if crash?.available && !crash.dismissed}<Badge tone="warn">New</Badge>{/if}
  {/snippet}
  {#if !crash}
    <LoadState error={loadError} what="crash reports" />
  {:else if crash.available && !crash.dismissed}
    <div class="stack">
      <dl class="facts">
        <dt>File</dt>
        <dd class="mono">{crash.filename}</dd>
        <dt>Process</dt>
        <dd>{crash.process}</dd>
        <dt>Captured</dt>
        <dd>{localTime(crash.captured_at)} <span class="muted">({age(crash.age_seconds)})</span></dd>
        <dt>Size</dt>
        <dd class="num">{bytes(crash.size_bytes)}</dd>
      </dl>
      <p class="muted note">The support bundle contains this crash report and the host logs. Attach it to a bug report.</p>
      <div class="row">
        <Button href={api.logs.supportBundleUrl} icon="download">Download support bundle</Button>
        <Button {busy} onclick={dismiss}>Dismiss</Button>
      </div>
    </div>
  {:else if crash.available}
    <p class="muted note">No new crash reports. You dismissed the last one, from {localTime(crash.captured_at)}.</p>
  {:else}
    <p class="muted note">No crash reports from the last 7 days.</p>
  {/if}
</Panel>

<style>
  .note {
    font-size: var(--text-sm);
  }
</style>

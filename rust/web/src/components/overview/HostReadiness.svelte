<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import type { Metadata } from '../../lib/api';
  import { link } from '../../lib/router.svelte';

  let {
    metadata,
    error,
    checking,
    onrecheck,
  }: { metadata: Metadata | null; error: string | null; checking: boolean; onrecheck: () => void } = $props();

  type Tone = 'neutral' | 'ok' | 'warn' | 'danger';
  interface Check {
    title: string;
    tone: Tone;
    state: string;
    text: string;
    /** The host's own error text, shown as is. */
    detail?: string;
    href: string;
    settings: string;
  }

  const list = new Intl.ListFormat('en', { style: 'long', type: 'conjunction' });
  const plural = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

  const CAPTURE: Record<string, string> = {
    wgc: 'Windows Graphics Capture',
    dxgi: 'Desktop Duplication',
    ddx: 'Desktop Duplication',
  };

  function encoder(meta: Metadata): Check {
    const base = { title: 'Video encoder', href: '/settings/video', settings: 'Video settings' };
    const status = meta.encoder_status;
    const warning = meta.warnings?.find((warning) => warning.code === 'video_encoder');
    if (warning) {
      return { ...base, tone: 'danger', state: 'Retrying', text: warning.message };
    }
    if (status.state === 'checking') {
      return { ...base, tone: 'neutral', state: 'Checking', text: 'Testing which codecs the graphics card can encode.' };
    }
    const codecs = [
      status.h264 && 'H.264',
      status.hevc && 'HEVC',
      status.av1 && 'AV1',
      status.pyrowave && 'PyroWave',
    ].filter((codec): codec is string => Boolean(codec));
    if (status.state === 'failed' || !codecs.length) {
      return {
        ...base,
        tone: 'danger',
        state: 'Unavailable',
        text: 'No video encoder available. Retrying.',
      };
    }
    if (!status.h264) {
      return {
        ...base,
        tone: 'warn',
        state: 'Limited',
        text: `Encodes ${list.format(codecs)}. Devices that only decode H.264 can't stream.`,
      };
    }
    return { ...base, tone: 'ok', state: 'Ready', text: `Encodes ${list.format(codecs)}.` };
  }

  function virtualDisplay(meta: Metadata): Check {
    const base = { title: 'Virtual display', href: '/settings/display', settings: 'Display settings' };
    const driver = meta.virtual_display;
    const on = meta.capture_status.virtual_display_configured;
    const detail = driver.reason || undefined;
    if (driver.ready) {
      return on
        ? { ...base, tone: 'ok', state: 'Ready', text: 'Streams use a virtual display.' }
        : { ...base, tone: 'neutral', state: 'Off', text: 'The driver is ready. Streams use a physical display.' };
    }
    if (driver.capable) {
      return { ...base, tone: 'warn', state: 'Not ready', text: "The driver is installed but isn't ready yet.", detail };
    }
    if (on) {
      return {
        ...base,
        tone: 'danger',
        state: 'Unavailable',
        text: "It's turned on, but the driver can't be opened. Check that the Butterpollo service is running, or use a physical display.",
        detail,
      };
    }
    return { ...base, tone: 'neutral', state: 'Off', text: 'Not available. Streams use a physical display.', detail };
  }

  function audio(meta: Metadata): Check {
    const base = { title: 'Audio', href: '/settings/audio', settings: 'Audio settings' };
    if (!meta.audio_enabled) {
      return { ...base, tone: 'neutral', state: 'Off', text: 'Audio streaming is turned off. Streams have no sound.' };
    }
    if (meta.audio_error) {
      return { ...base, tone: 'danger', state: 'Error', text: "Audio outputs can't be read.", detail: meta.audio_error };
    }
    const sinks = meta.audio_sinks ?? [];
    if (!sinks.length) {
      return { ...base, tone: 'warn', state: 'No output', text: 'No audio output was found, so streams have no sound.' };
    }
    const fallback = sinks.find((sink) => sink.default);
    const name = fallback ? fallback.name || fallback.description : '';
    return {
      ...base,
      tone: 'ok',
      state: 'Ready',
      text: `${plural(sinks.length, 'output', 'outputs')} found.${name ? ` Default is ${name}.` : ''}`,
    };
  }

  function capture(meta: Metadata): Check {
    const base = { title: 'Screen capture', href: '/settings/video', settings: 'Video settings' };
    const status = meta.capture_status;
    const backend = status.configured_backend || 'auto';
    const method =
      backend === 'auto' ? 'Picks the capture method automatically.' : `Uses ${CAPTURE[backend] ?? backend}.`;
    if (status.error) {
      return { ...base, tone: 'danger', state: 'Error', text: `${method} Displays can't be read.`, detail: status.error };
    }
    const displays = status.displays?.length ?? 0;
    if (displays) {
      return { ...base, tone: 'ok', state: 'Ready', text: `${method} ${plural(displays, 'display', 'displays')} found.` };
    }
    if (status.virtual_display_configured && meta.virtual_display.ready) {
      return { ...base, tone: 'ok', state: 'Ready', text: `${method} No monitor is on; streams use the virtual display.` };
    }
    return {
      ...base,
      tone: 'warn',
      state: 'No display',
      text: `${method} No active display was found. Turn on a monitor or set up a virtual display.`,
    };
  }

  const checks = $derived(metadata ? [encoder(metadata), virtualDisplay(metadata), audio(metadata), capture(metadata)] : []);
  const attention = $derived(checks.filter((check) => check.tone === 'warn' || check.tone === 'danger').length);
  const summary = $derived(
    !metadata
      ? undefined
      : attention
        ? `${plural(attention, 'check needs', 'checks need')} attention.`
        : 'Ready to stream.',
  );
</script>

<Panel title="Host readiness" description={summary} flush>
  {#snippet actions()}
    <Button size="sm" variant="ghost" icon="refresh" busy={checking} onclick={onrecheck}>Recheck</Button>
  {/snippet}
  {#if error}
    <p class="notice danger error" role="alert">Host details didn't load: {error}</p>
  {/if}
  {#if !metadata}
    {#if !error}<p class="placeholder muted">Checking the host…</p>{/if}
  {:else}
    <ul>
      {#each checks as check (check.title)}
        <li>
          <div class="head">
            <h3>{check.title}</h3>
            <Badge tone={check.tone}>{check.state}</Badge>
          </div>
          <p>{check.text}</p>
          {#if check.detail}<p class="detail mono">{check.detail}</p>{/if}
          <a href={check.href} use:link>{check.settings}</a>
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: grid;
    gap: 3px;
    padding: var(--space-3) var(--space-5);
    font-size: var(--text-sm);
  }
  li + li {
    border-top: 1px solid var(--line);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-2);
  }
  h3 {
    font-size: var(--text);
  }
  p {
    color: var(--ink-2);
  }
  .detail {
    color: var(--muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  a {
    justify-self: start;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  a:hover {
    color: var(--ink);
  }
  .placeholder {
    padding: var(--space-4) var(--space-5);
    font-size: var(--text-sm);
  }
  .error {
    margin: var(--space-3) var(--space-5);
  }
</style>

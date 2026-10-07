<script lang="ts">
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import { CODEC_NAMES, type StreamSession } from '../../lib/api';
  import { duration, ms } from '../../lib/format';
  import Sparkline from './Sparkline.svelte';

  let {
    stream,
    busy = false,
    ondisconnect,
  }: { stream: StreamSession; busy?: boolean; ondisconnect: (stream: StreamSession) => void } = $props();

  const stopping = $derived(stream.state === 'STOPPING');
  const perf = $derived(stream.performance);
  // No frames in the last two seconds: the averages are zeros, not measurements.
  const measured = $derived(perf.sample_frames > 0);
  const codec = $derived((CODEC_NAMES as readonly string[])[stream.video_format] ?? `Codec ${stream.video_format}`);
  const mbps = $derived(stream.encoder_bitrate_kbps / 1000);
  const starved = $derived(
    stream.pyrowave_minimum_kbps != null && stream.encoder_bitrate_kbps < stream.pyrowave_minimum_kbps,
  );
  const belowRecommended = $derived(
    stream.pyrowave_recommended_kbps != null && stream.encoder_bitrate_kbps < stream.pyrowave_recommended_kbps,
  );
  const minimum = $derived(Math.ceil((stream.pyrowave_minimum_kbps ?? 0) / 1000));
  const recommended = $derived(Math.ceil((stream.pyrowave_recommended_kbps ?? 0) / 1000));
  const trend = $derived(perf.history.map((sample) => sample.host_processing_mean_ms));
  const peak = $derived(trend.length ? Math.max(...trend) : 0);
  const latest = $derived(trend.at(-1) ?? 0);

  const timing = (value: number) => (measured ? ms(value) : '–');
</script>

<article class="stream" aria-label="Stream to {stream.device_name}">
  <header>
    <div class="who">
      <h3>{stream.device_name || 'Unnamed device'}</h3>
      {#if stopping}
        <Badge tone="warn">Stopping</Badge>
      {:else}
        <Badge tone="live">Live</Badge>
      {/if}
    </div>
    <Button size="sm" variant="danger" {busy} disabled={stopping} onclick={() => ondisconnect(stream)}>Disconnect</Button>
  </header>

  <ul class="spec">
    <li class="num">{stream.width}×{stream.height} at {stream.fps} fps</li>
    <li>{codec}</li>
    {#if stream.hdr}<li><Badge>HDR</Badge></li>{/if}
    {#if stream.vrr}<li><Badge>VRR</Badge></li>{/if}
    <li class="num">{mbps.toFixed(mbps < 100 ? 1 : 0)} Mbps</li>
    <li><span>Up <span class="num">{duration(stream.uptime_seconds)}</span></span></li>
  </ul>

  {#if starved || belowRecommended}
    <p class="note warn" class:danger={starved} role="status">
      {#if starved}
        PyroWave bitrate is too low: below <span class="num">{minimum}</span> Mbps, severe detail loss is likely.
      {:else}
        PyroWave bitrate is below recommended: text and textures may lose detail.
      {/if}
      Try <span class="num">{recommended}</span> Mbps or more at this resolution and frame rate, with network headroom.
      Quality depends on the picture. Raise the bitrate in Moonlight, or use HEVC or AV1.
    </p>
  {/if}

  <dl class="metrics">
    <div>
      <dt>Frame rate</dt>
      <dd class="value num">{measured ? `${Math.round(perf.fps)} fps` : '–'}</dd>
      <dd class="sub">target <span class="num">{stream.fps}</span></dd>
    </div>
    <div>
      <dt>Encode</dt>
      <dd class="value num">{timing(perf.encode_p95_ms)}</dd>
      <dd class="sub">p95</dd>
    </div>
    <div>
      <dt>Host processing</dt>
      <dd class="value num">{timing(perf.host_processing_mean_ms)}</dd>
      <dd class="sub">mean, p99 <span class="num">{timing(perf.host_processing_p99_ms)}</span></dd>
    </div>
    <div>
      <dt>Frame age</dt>
      <dd class="value num">{timing(perf.frame_age_mean_ms)}</dd>
      <dd class="sub">mean</dd>
    </div>
  </dl>
  {#if !measured && !stopping}
    <p class="note muted">No frames sent in the last 2 seconds.</p>
  {/if}

  <figure>
    <figcaption>
      <span>Host processing mean{trend.length > 1 ? `, last ${duration(trend.length)}` : ''}</span>
      {#if trend.length > 1}<span class="peak num">peak {ms(peak)}</span>{/if}
    </figcaption>
    {#if trend.length > 1}
      <Sparkline
        values={trend}
        label="Host processing over the last {duration(trend.length)}: now {ms(latest)}, peak {ms(peak)}"
      />
    {:else}
      <p class="collecting muted">Collecting samples…</p>
    {/if}
  </figure>
</article>

<style>
  .stream {
    container-type: inline-size;
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  h3 {
    font-size: var(--text-lg);
    overflow-wrap: anywhere;
  }
  .spec {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 0;
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: var(--text-sm);
    color: var(--ink-2);
  }
  .spec li {
    display: flex;
    align-items: center;
  }
  .spec li:not(:last-child)::after {
    content: '·';
    margin: 0 8px;
    color: var(--muted);
  }
  .metrics {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: hidden;
  }
  @container (max-width: 520px) {
    .metrics {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
  .metrics div {
    padding: var(--space-2) var(--space-3);
    box-shadow:
      1px 0 0 var(--line),
      0 1px 0 var(--line);
  }
  dt {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .value {
    font-size: var(--text-lg);
    line-height: 1.3;
  }
  .sub {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .note {
    font-size: var(--text-sm);
  }
  .warn {
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    background: var(--warn-soft);
    color: var(--warn);
  }
  .danger {
    background: var(--danger-soft);
    color: var(--danger);
  }
  figure {
    margin: 0;
    display: grid;
    gap: 4px;
  }
  figcaption {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .peak {
    white-space: nowrap;
  }
  .collecting {
    height: 40px;
    display: flex;
    align-items: center;
    font-size: var(--text-sm);
  }
</style>

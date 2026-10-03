<script lang="ts">
  let {
    values,
    label,
    slots = 120,
  }: {
    values: number[];
    /** Read out instead of the drawing. */
    label: string;
    /** Points across the full width; fewer values are drawn against the right edge. */
    slots?: number;
  } = $props();

  const width = 240;
  const height = 40;

  const recent = $derived(values.slice(-slots).map((value) => (Number.isFinite(value) ? Math.max(0, value) : 0)));
  const top = $derived(Math.max(1, ...recent) * 1.15);
  const points = $derived(
    recent
      .map((value, index) => {
        const x = ((slots - recent.length + index) / (slots - 1)) * width;
        const y = height - 1 - (value / top) * (height - 2);
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(' '),
  );
</script>

<svg viewBox="0 0 {width} {height}" preserveAspectRatio="none" role="img" aria-label={label}>
  <line x1="0" y1={height - 0.5} x2={width} y2={height - 0.5} vector-effect="non-scaling-stroke" />
  <polyline {points} vector-effect="non-scaling-stroke" />
</svg>

<style>
  svg {
    display: block;
    width: 100%;
    height: 40px;
    overflow: visible;
  }
  line {
    stroke: var(--line);
    stroke-width: 1;
  }
  polyline {
    fill: none;
    stroke: var(--ink-2);
    stroke-width: 1.5;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
</style>

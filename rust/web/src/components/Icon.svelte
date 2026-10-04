<script lang="ts" module>
  // 24-pixel stroke icons, drawn for the console.
  const paths = {
    overview: 'M3 12h4l2.5-6 5 12 2.5-6H21',
    library: 'M4 5h5v14H4zM11 5h4v14h-4zM17 6l3 .5-2 12.5-3-.5',
    devices: 'M8 3h8v18H8zM11 18h2M3 9h2M3 13h2M19 9h2M19 13h2',
    settings: 'M4 7h9M17 7h3M4 17h3M11 17h9M15 5v4M9 15v4',
    logs: 'M5 4h14v16H5zM8 8h8M8 12h8M8 16h5',
    maintenance: 'M14.5 6.5a4 4 0 0 0-5.3 5.3L4 17l3 3 5.2-5.2a4 4 0 0 0 5.3-5.3l-2.6 2.6-2.4-.6-.6-2.4z',
    key: 'M14 10a4 4 0 1 0-3.5 4L9 15.5V18H6.5v2.5H4V18l6-6M15.5 8.5h.01',
    play: 'M8 5l11 7-11 7z',
    stop: 'M7 7h10v10H7z',
    plus: 'M12 5v14M5 12h14',
    trash: 'M5 7h14M10 7V4h4v3M7 7l1 13h8l1-13',
    edit: 'M4 20h4L19 9l-4-4L4 16zM13 7l4 4',
    search: 'M10.5 17a6.5 6.5 0 1 1 0-13 6.5 6.5 0 0 1 0 13zM15.5 15.5 20 20',
    check: 'M5 12.5l4.5 4.5L19 7',
    x: 'M6 6l12 12M18 6 6 18',
    alert: 'M12 4 2.5 20h19zM12 10v4.5M12 17.5h.01',
    link: 'M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1',
    power: 'M12 3v8M7 6.5a7 7 0 1 0 10 0',
    sun: 'M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM12 2v2M12 20v2M4 12H2M22 12h-2M5 5l1.5 1.5M17.5 17.5 19 19M5 19l1.5-1.5M17.5 6.5 19 5',
    moon: 'M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z',
    monitor: 'M3 5h18v11H3zM9 20h6M12 16v4',
    gamepad: 'M7 8h10a4 4 0 0 1 4 4v1a3 3 0 0 1-5.5 1.7L14.7 13H9.3l-.8 1.7A3 3 0 0 1 3 13v-1a4 4 0 0 1 4-4zM8 10.5v3M6.5 12h3M15.5 11h.01M17.5 13h.01',
    refresh: 'M20 11a8 8 0 0 0-14.8-3.5M4 4v4h4M4 13a8 8 0 0 0 14.8 3.5M20 20v-4h-4',
    download: 'M12 4v11M7 10l5 5 5-5M5 20h14',
    external: 'M14 4h6v6M20 4l-9 9M18 14v6H4V6h6',
    chevron: 'M9 6l6 6-6 6',
    menu: 'M4 7h16M4 12h16M4 17h16',
    signout: 'M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10',
    image: 'M4 5h16v14H4zM4 16l5-5 4 4 2-2 5 5M15.5 9.5h.01',
    upload: 'M12 16V5M7 10l5-5 5 5M5 20h14',
    copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
  } as const;
  export type IconName = keyof typeof paths;
</script>

<script lang="ts">
  let { name, size = 18, label }: { name: IconName; size?: number; label?: string } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? 'img' : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : 'true'}
>
  <path d={paths[name]} />
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>

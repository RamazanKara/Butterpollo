// Formatting shared by the pages.

export function duration(seconds: number): string {
  if (seconds < 60) return `${Math.floor(seconds)} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  return `${hours} h ${minutes % 60} min`;
}

export function bytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MB`;
  return `${(value / 1024 ** 3).toFixed(2)} GB`;
}

export function ms(value: number): string {
  return `${value.toFixed(value < 10 ? 1 : 0)} ms`;
}

/** Unix seconds as a local date and time. */
export function when(unix: number): string {
  if (!unix) return 'never';
  return new Date(unix * 1000).toLocaleString();
}

/** "3 min ago" for recent times, otherwise the date. */
export function ago(unix: number): string {
  const seconds = Date.now() / 1000 - unix;
  if (seconds < 90) return 'just now';
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return new Date(unix * 1000).toLocaleDateString();
}

/** Poll `task` every `interval` ms while the page is visible; returns a stop function. */
export function poll(task: () => Promise<unknown> | void, interval: number): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  const run = async () => {
    if (stopped) return;
    if (document.visibilityState === 'visible') {
      try {
        await task();
      } catch {
        // The page shows its own error state.
      }
    }
    timer = setTimeout(run, interval);
  };
  void run();
  return () => {
    stopped = true;
    if (timer) clearTimeout(timer);
  };
}

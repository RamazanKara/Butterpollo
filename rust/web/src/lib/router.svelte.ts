// History routing for the console's handful of pages.

export const route = $state({ path: location.pathname, search: location.search });

/** Asked before leaving the page in the app (e.g. unsaved changes). */
type Guard = (to: string) => boolean | Promise<boolean>;
let guard: Guard | null = null;

/** Install a guard; returns a function that removes it. */
export function guardNavigation(check: Guard): () => void {
  guard = check;
  return () => {
    if (guard === check) guard = null;
  };
}

function sync() {
  route.path = location.pathname;
  route.search = location.search;
}
window.addEventListener('popstate', sync);

function go(to: string, replace: boolean) {
  if (replace) history.replaceState(null, '', to);
  else history.pushState(null, '', to);
  sync();
  window.scrollTo(0, 0);
}

export function navigate(to: string, options: { replace?: boolean; force?: boolean } = {}) {
  if (to === location.pathname + location.search) return;
  if (!guard || options.force) {
    go(to, options.replace ?? false);
    return;
  }
  void Promise.resolve(guard(to)).then((leave) => {
    if (leave) {
      guard = null;
      go(to, options.replace ?? false);
    }
  });
}

/** Match `/library/:uuid` style patterns; returns the parameters or null. */
export function match(pattern: string, path = route.path): Record<string, string> | null {
  const want = pattern.split('/').filter(Boolean);
  const have = path.split('/').filter(Boolean);
  if (want.length !== have.length) return null;
  const params: Record<string, string> = {};
  for (const [index, part] of want.entries()) {
    const value = have[index] ?? '';
    if (part.startsWith(':')) params[part.slice(1)] = decodeURIComponent(value);
    else if (part !== value) return null;
  }
  return params;
}

export function query(name: string): string | null {
  return new URLSearchParams(route.search).get(name);
}

/** Use on <a href> elements: same-origin clicks stay in the page. */
export function link(node: HTMLAnchorElement) {
  const onclick = (event: MouseEvent) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const url = new URL(node.href, location.href);
    if (url.origin !== location.origin || node.target || url.pathname.startsWith('/api/')) return;
    event.preventDefault();
    navigate(url.pathname + url.search);
  };
  node.addEventListener('click', onclick);
  return { destroy: () => node.removeEventListener('click', onclick) };
}

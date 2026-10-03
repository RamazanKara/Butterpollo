// Who is signed in, and what the host is: shared by the shell and pages.
import { api, type Metadata, type PendingPairing } from './api';
import { navigate, route } from './router.svelte';

export const session = $state({
  /** checking → signed-out | needs-setup | signed-in */
  state: 'checking' as 'checking' | 'signed-out' | 'needs-setup' | 'signed-in',
  metadata: null as Metadata | null,
  pending: [] as PendingPairing[],
});

const PUBLIC = ['/login', '/setup'];

export async function checkSession() {
  try {
    const status = await api.auth.status();
    if (!status.credentials_configured) {
      session.state = 'needs-setup';
      if (route.path !== '/setup') navigate('/setup', { replace: true });
      return;
    }
    if (!status.authenticated) {
      signedOut();
      return;
    }
    await api.auth.resume();
    session.state = 'signed-in';
    if (PUBLIC.includes(route.path)) navigate('/', { replace: true });
    void refreshMetadata();
  } catch {
    session.state = 'signed-out';
  }
}

export function signedOut() {
  session.state = 'signed-out';
  session.metadata = null;
  if (route.path !== '/login') {
    const back = route.path === '/' ? '' : `?next=${encodeURIComponent(route.path + route.search)}`;
    navigate(`/login${back}`, { replace: true });
  }
}

export async function refreshMetadata() {
  session.metadata = await api.metadata();
}

/** Pairing requests waiting for a PIN, shown on every page. */
export async function refreshPending() {
  if (session.state !== 'signed-in') return;
  session.pending = (await api.clients.pending()).requests;
}

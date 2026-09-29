import { apiGet, apiPost } from '@/services/api';
import type { GitHubReleaseLike } from '@/utils/changelog';

/** Result of the host's release checks; the browser never queries GitHub itself. */
export interface UpdateStatus {
  checking: boolean;
  checkFailed: boolean;
  /** Releases from the last successful check, in GitHub's format. */
  releases: GitHubReleaseLike[];
}

interface UpdateStatusPayload {
  checking?: unknown;
  check_failed?: unknown;
  releases?: unknown;
}

// The host gives up on GitHub after 40 s (connect plus transfer timeout).
const CHECK_TIMEOUT_MS = 45000;
const CHECK_POLL_MS = 1000;

export async function fetchUpdateStatus(signal?: AbortSignal): Promise<UpdateStatus> {
  const payload = await apiGet<UpdateStatusPayload | null>('/api/updates', { signal });
  return {
    checking: payload?.checking === true,
    checkFailed: payload?.check_failed === true,
    releases: Array.isArray(payload?.releases) ? (payload.releases as GitHubReleaseLike[]) : [],
  };
}

/** Ask the host to check for releases now and wait for the result. */
export async function checkForUpdates(signal?: AbortSignal): Promise<UpdateStatus> {
  await apiPost('/api/updates/check');
  const deadline = Date.now() + CHECK_TIMEOUT_MS;
  let status = await fetchUpdateStatus(signal);
  while (status.checking) {
    if (Date.now() >= deadline) throw new Error('update-check-timeout');
    await new Promise((resolve) => setTimeout(resolve, CHECK_POLL_MS));
    status = await fetchUpdateStatus(signal);
  }
  return status;
}

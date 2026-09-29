import {
  compareChangelogTags,
  githubReleaseToChangelogEntry,
  parseChangelogVersion,
  type ChangelogEntry,
  type GitHubReleaseLike,
} from './changelog.ts';
import { PROJECT_REPOSITORY_URL } from './project.ts';

export function selectAvailableUpdate(
  installedVersion: string,
  releases: GitHubReleaseLike[],
  notifyPrereleases: unknown,
): ChangelogEntry | null {
  if (!/^v?\d+\.\d+\.\d+(?:[-+].*)?$/i.test(installedVersion)) return null;
  const allowPrereleases =
    notifyPrereleases === true ||
    notifyPrereleases === 'enabled' ||
    parseChangelogVersion(installedVersion).channel !== 'stable';
  let best: ChangelogEntry | null = null;
  for (const release of releases) {
    const entry = githubReleaseToChangelogEntry(release);
    if (!entry || (entry.prerelease && !allowPrereleases)) continue;
    if (compareChangelogTags(entry.tag, installedVersion) <= 0) continue;
    if (!best || compareChangelogTags(entry.tag, best.tag) > 0) best = entry;
  }
  return best;
}

/** Link for a release: its GitHub page, or the project's release list. */
export function releasePageUrl(entry: Pick<ChangelogEntry, 'url'> | null): string {
  return entry?.url?.startsWith('https://github.com/')
    ? entry.url
    : `${PROJECT_REPOSITORY_URL}/releases`;
}

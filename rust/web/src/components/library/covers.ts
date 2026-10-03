// Cover art: IGDB covers found through LizardByte's GameDB, searched from the
// browser, and images from this computer converted to PNG.

const GAMEDB = 'https://raw.githubusercontent.com/LizardByte/GameDB/gh-pages';
const IGDB = 'https://images.igdb.com/igdb/image/upload';
/** Game files fetched per search. */
const LIMIT = 40;
/** The host reads at most 1 MiB of request body; leave room for the JSON around the image. */
const MAX_BASE64 = 1_000_000;

export interface CoverCandidate {
  name: string;
  /** Cover file name on the host. */
  key: string;
  thumb: string;
  /** The PNG the host downloads. */
  saveUrl: string;
}

interface Game {
  id?: number | string;
  name?: string;
  cover?: { url?: string };
}

// Buckets are large and rarely change; keep them for the session.
const buckets = new Map<string, Promise<Record<string, { name?: string }>>>();

async function fetchJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(url, { signal, credentials: 'omit' });
  if (!response.ok) throw new Error(`GameDB answered ${response.status} ${response.statusText}`.trim());
  return (await response.json()) as T;
}

/** "Half Life 2" → "half.life.2", as GameDB compares names. */
function searchable(name: string): string {
  return name.trim().replace(/\s+/g, '.').toLowerCase();
}

export function bucketOf(query: string): string {
  return (
    query
      .trim()
      .slice(0, 2)
      .toLowerCase()
      .replace(/[^a-z0-9]/g, '') || '@'
  );
}

function bucket(prefix: string): Promise<Record<string, { name?: string }>> {
  let entry = buckets.get(prefix);
  if (!entry) {
    entry = fetch(`${GAMEDB}/buckets/${prefix}.json`, { credentials: 'omit' }).then(async (response) => {
      // No bucket means no game starts that way.
      if (response.status === 404) return {};
      if (!response.ok) throw new Error(`GameDB answered ${response.status} ${response.statusText}`.trim());
      return (await response.json()) as Record<string, { name?: string }>;
    });
    entry.catch(() => buckets.delete(prefix));
    buckets.set(prefix, entry);
  }
  return entry;
}

export async function searchCovers(query: string, signal: AbortSignal): Promise<CoverCandidate[]> {
  const wanted = searchable(query);
  if (!wanted) return [];
  const games = await bucket(bucketOf(query));
  signal.throwIfAborted();
  // Exact and shorter names first, so "Halo" is not crowded out by its sequels.
  const ids = Object.entries(games)
    .flatMap(([id, game]) => {
      const name = typeof game?.name === 'string' ? searchable(game.name) : '';
      return name.startsWith(wanted) ? [{ id, name }] : [];
    })
    .sort((a, b) => Number(b.name === wanted) - Number(a.name === wanted) || a.name.length - b.name.length)
    .slice(0, LIMIT)
    .map((entry) => entry.id);

  const fetched = await Promise.allSettled(
    ids.map((id) => fetchJson<Game>(`${GAMEDB}/games/${encodeURIComponent(id)}.json`, signal)),
  );
  signal.throwIfAborted();
  const candidates: CoverCandidate[] = [];
  for (const [index, result] of fetched.entries()) {
    if (result.status !== 'fulfilled') continue;
    const game = result.value;
    const url = game.cover?.url;
    if (typeof url !== 'string') continue;
    const slash = url.lastIndexOf('/');
    const dot = url.lastIndexOf('.');
    if (slash < 0 || dot <= slash + 1) continue;
    const slug = url.slice(slash + 1, dot);
    const key = `igdb_${game.id ?? ids[index]}`;
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(key)) continue;
    candidates.push({
      name: typeof game.name === 'string' ? game.name : '',
      key,
      thumb: `${IGDB}/t_cover_big/${slug}.jpg`,
      saveUrl: `${IGDB}/t_cover_big_2x/${slug}.png`,
    });
  }
  return candidates;
}

function readFile(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error('The file could not be read.'));
    reader.readAsDataURL(file);
  });
}

function decode(source: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error('The browser can’t open this image.'));
    image.src = source;
  });
}

/**
 * The image scaled down to fit 600×900, as PNG. `data` is base64 without the
 * data: prefix; `preview` is the full data URL.
 */
export async function coverFromFile(file: File): Promise<{ data: string; preview: string }> {
  const image = await decode(await readFile(file));
  const { naturalWidth: width, naturalHeight: height } = image;
  if (!width || !height) throw new Error('The image has no size.');
  let scale = Math.min(1, 600 / width, 900 / height);
  const canvas = document.createElement('canvas');
  const context = canvas.getContext('2d');
  if (!context) throw new Error('The browser can’t draw the image.');
  for (let attempt = 0; ; attempt++) {
    canvas.width = Math.max(1, Math.round(width * scale));
    canvas.height = Math.max(1, Math.round(height * scale));
    context.clearRect(0, 0, canvas.width, canvas.height);
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
    const preview = canvas.toDataURL('image/png');
    const data = preview.replace(/^data:image\/png;base64,/, '');
    // Detailed photos make large PNGs; shrink until the host accepts the request.
    if (data.length <= MAX_BASE64) return { data, preview };
    if (attempt >= 6) throw new Error('The image is too detailed to store as a cover. Try a smaller one.');
    scale *= 0.8;
  }
}

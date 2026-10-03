// Typed client for the Butterpollo admin API (/api on the console port).
// Sessions live in HttpOnly cookies; writes carry the session's CSRF token.

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

let csrfToken = '';
let onSignedOut: (() => void) | null = null;

/** Called when the session can no longer be refreshed. */
export function setSignedOutHandler(handler: () => void) {
  onSignedOut = handler;
}
export function setCsrfToken(token: string) {
  csrfToken = token;
}

async function refreshCsrf(): Promise<void> {
  const response = await fetch('/api/csrf-token', { credentials: 'same-origin' });
  if (response.ok) {
    const body = (await response.json()) as { csrf_token: string };
    csrfToken = body.csrf_token;
  }
}

async function refreshSession(): Promise<boolean> {
  const response = await fetch('/api/auth/refresh', { method: 'POST', credentials: 'same-origin' });
  if (!response.ok) return false;
  const body = (await response.json()) as IssuedSession;
  csrfToken = body.csrf_token;
  return true;
}

type Method = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

async function send(method: Method, path: string, body?: unknown, retried = false): Promise<Response> {
  const headers: Record<string, string> = {};
  if (method !== 'GET') {
    if (!csrfToken) await refreshCsrf();
    headers['X-CSRF-Token'] = csrfToken;
  }
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const response = await fetch(path, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (retried || path.startsWith('/api/auth/')) return response;
  if (response.status === 401) {
    if (await refreshSession()) return send(method, path, body, true);
    onSignedOut?.();
    return response;
  }
  if (response.status === 400 && method !== 'GET') {
    // CSRF tokens are regenerated when the host restarts.
    const text = await response.clone().text();
    if (text.includes('CSRF token required')) {
      await refreshCsrf();
      return send(method, path, body, true);
    }
  }
  return response;
}

async function json<T>(method: Method, path: string, body?: unknown): Promise<T> {
  const response = await send(method, path, body);
  const text = await response.text();
  let parsed: unknown = undefined;
  try {
    parsed = text ? JSON.parse(text) : undefined;
  } catch {
    // Some errors are plain text.
  }
  if (!response.ok) {
    const message =
      (parsed as { error?: string } | undefined)?.error ?? (text || `${response.status} ${response.statusText}`);
    throw new ApiError(response.status, message);
  }
  return parsed as T;
}

const get = <T>(path: string) => json<T>('GET', path);
const post = <T>(path: string, body: unknown = {}) => json<T>('POST', path, body);

// ---------- types ----------

export interface Ok {
  status: true;
}
export interface AuthStatus {
  status: true;
  authenticated: boolean;
  credentials_configured: boolean;
  login_required: boolean;
}
export interface IssuedSession {
  status: true;
  access_token: string;
  refresh_token: string;
  csrf_token: string;
  expires_in: number;
  refresh_expires_in: number;
  remember_me: boolean;
}
export interface WebSession {
  id: string;
  current: boolean;
  username: string;
  created_at: number;
  expires_at: number;
  refresh_expires_at: number;
  last_seen: number;
  remember_me: boolean;
  user_agent: string;
  remote_address: string;
  device_label: string;
}
export interface TokenScope {
  path: string;
  methods: string[];
}
export interface ApiToken {
  hash: string;
  username: string;
  created_at: number | string;
  scopes: TokenScope[];
}
export type ConfigValue = string | number | boolean | null | unknown[] | Record<string, unknown>;
export type Config = Record<string, ConfigValue>;

export interface CaptureDisplay {
  device_id: string;
  display_name: string;
  friendly_name: string;
  width: number;
  height: number;
  x: number;
  y: number;
  primary: boolean;
  adapter: string;
}
export interface AudioEndpoint {
  id: string;
  name: string;
  description: string;
  adapter: string;
  default: boolean;
  virtual_sink: boolean;
}
export interface Metadata {
  status: true;
  platform: 'windows';
  version: string;
  host_name: string;
  pc_address: string;
  pc_addresses: string[];
  paired_devices: number;
  encoder_status: { state: 'checking' | 'failed' | 'ready'; h264: boolean; hevc: boolean; av1: boolean; pyrowave: boolean };
  capture_status: {
    configured_backend: string;
    virtual_display_configured: boolean;
    displays: CaptureDisplay[] | null;
    error: string | null;
  };
  virtual_display: { capable: boolean; ready: boolean; reason: string; protocol: string };
  audio_sinks: AudioEndpoint[] | null;
  audio_error: string | null;
  audio_enabled: boolean;
  features: { truehdr_runtime: boolean; pyrowave: boolean; virtual_display: boolean; [key: string]: boolean };
  credentials_exists: boolean;
}

export interface PrepCommand {
  do: string;
  undo: string;
  elevated: boolean;
}
export interface App {
  name: string;
  cmd: string;
  'working-dir': string;
  'prep-cmd': PrepCommand[];
  uuid?: string;
  'image-path'?: string;
  detached?: string[];
  'config-overrides'?: Record<string, string | number | boolean>;
  [extra: string]: unknown;
}
export interface AppsDocument {
  status: true;
  apps: App[];
  env?: Record<string, string>;
}

export const PERM = {
  LIST_APPS: 0x01000000,
  VIEW_STREAMS: 0x02000000,
  LAUNCH_APPS: 0x04000000,
  CONTROLLER: 0x100,
  TOUCH: 0x200,
  PEN: 0x400,
  MOUSE: 0x800,
  KEYBOARD: 0x1000,
  CLIPBOARD_WRITE: 0x10000,
  CLIPBOARD_READ: 0x20000,
  FILE_UPLOAD: 0x40000,
  FILE_DOWNLOAD: 0x80000,
  SERVER_CMD: 0x100000,
} as const;
export const PERM_ALL = 0x071f1f00;

// Per-device settings: null clears one, so the host setting applies.
export interface Client {
  uuid: string;
  name: string;
  perm: number;
  enabled: boolean;
  connected: boolean;
  output_name_override?: string | null;
  display_mode?: string | null;
  always_use_virtual_display?: boolean | string | null;
  virtual_display_mode?: string | null;
  virtual_display_layout?: string | null;
  prefer_10bit_sdr?: boolean | string | null;
  hdr_profile?: string | null;
  allow_client_commands?: boolean | string | null;
  do?: unknown;
  undo?: unknown;
  [extra: string]: unknown;
}
export interface PendingPairing {
  uniqueid: string;
  name: string;
  age_seconds: number;
}

export interface SteamStatus {
  status: true;
  enabled: boolean;
  /** Whether Steam is installed on the host. */
  available: boolean;
  game_count: number;
  importable_game_count: number;
  selected_game_count: number;
  auto_sync: boolean;
  sync_all_installed: boolean;
  recent_games: number;
}

export interface PlayniteStatus {
  status: true;
  enabled: boolean;
  /** Playnite is running. */
  active: boolean;
  /** Playnite was found on the host. */
  available: boolean;
  /** The plugin is in Playnite's extensions. */
  installed: boolean;
  installed_version: string | null;
  packaged_version: string | null;
  update_available: boolean;
  auto_sync: boolean;
  game_count: number;
  synced_seconds_ago: number | null;
}

export interface PerfSample {
  fps: number;
  bitrate_mbps: number;
  encode_mean_ms: number;
  encode_max_ms: number;
  host_processing_mean_ms: number;
  host_processing_max_ms: number;
  frame_age_mean_ms: number;
}
export interface Performance {
  fps: number;
  bitrate_mbps: number;
  encode_mean_ms: number;
  encode_p95_ms: number;
  encode_p99_ms: number;
  host_processing_mean_ms: number;
  host_processing_p95_ms: number;
  host_processing_p99_ms: number;
  host_processing_max_ms: number;
  frame_age_mean_ms: number;
  frame_age_p95_ms: number;
  present_to_send_mean_ms: number;
  send_interval_p99_ms: number;
  sample_frames: number;
  history: PerfSample[];
}
export type Role = 'stream' | 'remote_monitor' | 'input_only';
export interface StreamSession {
  uuid: string;
  device_name: string;
  width: number;
  height: number;
  fps: number;
  video_format: 0 | 1 | 2 | 3;
  hdr: boolean;
  vrr: boolean;
  encoder_bitrate_kbps: number;
  audio_channels: number;
  state: 'RUNNING' | 'STOPPING';
  frames_sent: number;
  bytes_sent: number;
  idr_requests: number;
  performance: Performance;
  uptime_seconds: number;
  role: Role;
}
export interface SessionStatus {
  status: true;
  activeSessions: number;
  appRunning: boolean;
  appName: string;
  paused: boolean;
  running: boolean;
  app: { name: string; id: number } | null;
}
export interface DisplayDevice {
  device_id: string;
  display_name: string;
  friendly_name: string;
  hdr_supported: boolean;
  hdr_enabled: boolean;
  primary: boolean;
}
export interface GoldenStatus {
  status: true;
  exists: boolean;
  comparison_available: boolean;
  current_mismatch_reason: string;
}
export interface LimiterStatus {
  status: true;
  enabled: boolean;
  configured_provider: string;
  active_provider: string;
  nvidia_available: boolean;
  rtss_available: boolean;
  resolved_path: string;
  path_exists: boolean;
  process_running: boolean;
  message: string;
}
export interface VulkanLayerStatus {
  status: true;
  installed: boolean;
  enabled: boolean;
  available: boolean;
  active: boolean;
}
export type CrashStatus =
  | { available: false; dismissed: false }
  | {
      available: true;
      filename: string;
      process: string;
      size_bytes: number;
      captured_at: string;
      age_seconds: number;
      dismissed: boolean;
    };
export interface Release {
  tag_name: string;
  name: string;
  html_url: string;
  prerelease: boolean;
  published_at: string;
  body: string;
}
export interface UpdatesState {
  status: true;
  checking: boolean;
  check_failed: boolean;
  checked_at: number;
  releases: Release[];
}
export interface LogChunk {
  status: true;
  /** Byte offset to ask for next. */
  offset: number;
  size: number;
  /** The log was rotated or truncated since `offset`; `text` starts afresh. */
  reset: boolean;
  text: string;
}

// ---------- endpoints ----------

export const api = {
  auth: {
    status: () => get<AuthStatus>('/api/auth/status'),
    async login(username: string, password: string, rememberMe: boolean) {
      await refreshCsrf();
      const session = await post<IssuedSession>('/api/auth/login', { username, password, remember_me: rememberMe });
      csrfToken = session.csrf_token;
      return session;
    },
    logout: () => post<Ok>('/api/auth/logout'),
    async resume() {
      await refreshCsrf();
    },
    sessions: () => get<{ status: true; sessions: WebSession[] }>('/api/auth/sessions'),
    revokeSession: (id: string) => json<Ok>('DELETE', `/api/auth/sessions/${encodeURIComponent(id)}`),
    setPassword: (body: {
      currentUsername?: string;
      currentPassword?: string;
      newUsername: string;
      newPassword: string;
      confirmNewPassword: string;
    }) => post<Ok>('/api/password', body),
  },
  metadata: () => get<Metadata>('/api/metadata'),
  config: {
    get: () => get<Config>('/api/config'),
    /** Merge keys; null or "" resets a key to its default. */
    patch: (values: Config) => json<{ status: true; restart_required: boolean; warning: string | null }>('PATCH', '/api/config', values),
  },
  apps: {
    list: () => get<AppsDocument>('/api/apps'),
    /** Replaces the whole record; start from the object returned by list(). */
    save: (app: App) => post<{ status: true; uuid: string }>('/api/apps', app),
    remove: (uuid: string) => json<Ok>('DELETE', `/api/apps/${encodeURIComponent(uuid)}`),
    reorder: (order: string[]) => post<Ok>('/api/apps/reorder', { order }),
    launch: (uuid: string) => post<Ok>('/api/apps/launch', { uuid }),
    close: () => post<Ok>('/api/apps/close'),
    coverUrl: (uuid: string, version = '') => `/api/apps/${encodeURIComponent(uuid)}/cover${version ? `?v=${version}` : ''}`,
    /** Store a cover from an https PNG URL (IGDB) or a PNG as base64. */
    uploadCover: (key: string, source: { url: string } | { data: string }) =>
      post<{ status: true; path: string }>('/api/covers/upload', { key, ...source }),
    rtxLive: (uuid: string, overrides: Record<string, string | number | boolean>) =>
      post<{ status: true; applied: boolean }>('/api/apps/rtx_hdr/live', { uuid, 'config-overrides': overrides }),
  },
  steam: {
    status: () => get<SteamStatus>('/api/steam/status'),
    sync: () =>
      post<{ status: true; changed: boolean; game_count: number; importable_game_count: number }>(
        '/api/steam/force_sync',
        {},
      ),
  },
  playnite: {
    status: () => get<PlayniteStatus>('/api/playnite/status'),
    sync: () => post<{ status: true; changed: boolean; game_count: number }>('/api/playnite/force_sync', {}),
    install: () => post<{ status: true; path: string; restart_required: boolean }>('/api/playnite/install', {}),
  },
  clients: {
    list: () => get<{ status: true; clients: Client[] }>('/api/clients/list'),
    pending: () => get<{ status: true; requests: PendingPairing[] }>('/api/clients/pending'),
    update: (client: Partial<Client> & { uuid: string }) => {
      const { connected: _connected, ...rest } = client;
      return post<Ok>('/api/clients/update', rest);
    },
    unpair: (uuid: string) => post<Ok>('/api/clients/unpair', { uuid }),
    unpairAll: () => post<Ok>('/api/clients/unpair-all'),
    disconnect: (uuid: string) => post<Ok>('/api/clients/disconnect', { uuid }),
    pin: (pin: string, uniqueid?: string, name?: string) => post<Ok>('/api/pin', { pin, uniqueid, name }),
    otp: (passphrase: string, deviceName: string) =>
      post<{ status: true; otp: string; ip: string; name: string }>('/api/otp', { passphrase, deviceName }),
    hdrProfiles: () => get<{ status: true; profiles: { filename: string }[] }>('/api/clients/hdr-profiles'),
  },
  sessions: {
    status: () => get<SessionStatus>('/api/session/status'),
    streams: () => get<{ status: true; sessions: StreamSession[] }>('/api/rtsp/sessions'),
  },
  displays: {
    devices: () => get<DisplayDevice[]>('/api/display-devices'),
    goldenStatus: () => get<GoldenStatus>('/api/display/golden_status?compare_current=1'),
    exportGolden: () => post<Ok>('/api/display/export_golden'),
    restoreGolden: () => post<Ok>('/api/display/restore_golden'),
    deleteGolden: () => json<Ok>('DELETE', '/api/display/golden'),
    disconnectVirtual: () => post<Ok>('/api/display/terminate_virtual'),
    resetPersistence: () => post<Ok>('/api/reset-display-device-persistence'),
  },
  health: {
    limiter: () => get<LimiterStatus>('/api/frame-limiter/status'),
    vulkanLayer: () => get<VulkanLayerStatus>('/api/health/vulkan-hdr-layer'),
    registerVulkanLayer: () => post<VulkanLayerStatus>('/api/health/vulkan-hdr-layer/register'),
    crash: () => get<CrashStatus>('/api/health/crashdump'),
    dismissCrash: (filename: string, captured_at: string) =>
      post<Ok>('/api/health/crashdump/dismiss', { filename, captured_at }),
  },
  logs: {
    /** Bytes after `offset`, at most `max`; offset -1 means the last `max` bytes. */
    read: (offset: number, max = 256 * 1024) => get<LogChunk>(`/api/logs/tail?offset=${offset}&max=${max}`),
    downloadUrl: '/api/logs/export',
    supportBundleUrl: '/api/logs/export_crash',
  },
  updates: {
    state: () => get<UpdatesState>('/api/updates'),
    check: () => post<Ok>('/api/updates/check'),
  },
  tokens: {
    list: () => get<{ status: true; tokens: ApiToken[] }>('/api/tokens'),
    routes: () => get<{ status: true; routes: TokenScope[] }>('/api/token/routes'),
    create: (scopes: TokenScope[]) => post<{ status: true; token: string }>('/api/token', { scopes }),
    revoke: (hash: string) => json<Ok>('DELETE', `/api/token/${encodeURIComponent(hash)}`),
  },
  host: {
    restart: () => post<Ok>('/api/restart'),
    quit: () => post<Ok>('/api/quit'),
  },
};

export const CODEC_NAMES = ['H.264', 'HEVC', 'AV1', 'PyroWave'] as const;

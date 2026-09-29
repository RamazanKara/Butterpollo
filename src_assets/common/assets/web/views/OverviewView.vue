<script setup lang="ts">
import { supportsManagedLinuxDisplay } from '@/utils/providerCapabilities';
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';

import LinuxCaptureStatus from '@/components/settings/LinuxCaptureStatus.vue';
import { ApiError, apiGet, apiPost } from '@/services/api';
import {
  AppButton,
  ConfirmDialog,
  InlineAlert,
  LoadingSkeleton,
  PageHeader,
  UiIcon,
  type StatusTone,
} from '@/components/ui';
import type { SessionStatus } from '@/types/sessions';
import { useSystemStore, type HostMetadata } from '@/stores/system';

interface MutationResponse {
  status?: boolean | string;
  error?: string;
}

interface OverviewWarning {
  key: string;
  title: string;
  detail: string;
  to: string;
  action: string;
}

interface VigemHealth {
  status?: unknown;
  installed?: unknown;
  required?: unknown;
  version?: unknown;
}

const { locale, t } = useI18n();
const system = useSystemStore();
const session = ref<SessionStatus | null>(null);
const hostPlatform = ref('');
const vigemInstalled = ref<boolean | null>(null);
const vigemRequired = ref<boolean | null>(null);
const vigemVersion = ref('');
const controllerEnabled = ref<boolean | null>(null);
const loading = ref(true);
const refreshing = ref(false);
const fetchErrors = ref<string[]>([]);
const lastUpdatedAt = ref<number | null>(null);
const stopConfirmOpen = ref(false);
const stopping = ref(false);
const stopNotice = ref('');
const stopError = ref('');
let pollTimer: number | undefined;

function errorMessage(cause: unknown, fallback: string): string {
  return cause instanceof ApiError ? fallback : cause instanceof Error ? cause.message : fallback;
}

function isWindowsPlatform(value: unknown): boolean {
  const platform = String(value ?? '')
    .trim()
    .toLocaleLowerCase();
  return platform === 'windows' || platform.startsWith('win');
}

function enabledValue(value: unknown): boolean {
  if (typeof value === 'boolean') return value;
  if (typeof value === 'number') return value !== 0;
  if (typeof value === 'string') {
    const normalized = value.trim().toLocaleLowerCase();
    return ['1', 'true', 'yes', 'on', 'enabled'].includes(normalized);
  }
  return false;
}

async function refreshVigem(platform: string): Promise<void> {
  // ViGEm is Windows-only. Do not request its endpoint on another host.
  if (!isWindowsPlatform(platform)) {
    controllerEnabled.value = null;
    vigemInstalled.value = null;
    vigemRequired.value = null;
    vigemVersion.value = '';
    return;
  }

  try {
    const config = await apiGet<Record<string, unknown>>('/api/config');
    if (config.status === false) throw new Error('vigem-config-rejected');
    const enabled = enabledValue(config.controller);
    controllerEnabled.value = enabled;
    if (!enabled) {
      vigemInstalled.value = null;
      vigemRequired.value = null;
      vigemVersion.value = '';
      return;
    }

    const health = await apiGet<VigemHealth>('/api/health/vigem');
    // Only a successful, explicit boolean false means the driver is absent.
    // Auth, network, malformed, and unknown responses remain unknown.
    if (health && health.status !== false && typeof health.installed === 'boolean') {
      vigemInstalled.value = health.installed;
      vigemRequired.value = typeof health.required === 'boolean' ? health.required : null;
      vigemVersion.value = typeof health.version === 'string' ? health.version : '';
    } else {
      vigemInstalled.value = null;
      vigemRequired.value = null;
      vigemVersion.value = '';
    }
  } catch {
    controllerEnabled.value = null;
    vigemInstalled.value = null;
    vigemRequired.value = null;
    vigemVersion.value = '';
  }
}

async function refresh(silent = false): Promise<void> {
  if (refreshing.value) return;
  refreshing.value = true;
  if (!silent) loading.value = true;

  void system.refreshHost();
  const results = await Promise.allSettled([
    apiGet<SessionStatus>('/api/session/status'),
    apiGet<Pick<HostMetadata, 'platform'>>('/api/metadata'),
  ]);

  const nextErrors: string[] = [];
  const [sessionResult, metadataResult] = results;

  if (sessionResult.status === 'fulfilled') {
    session.value = sessionResult.value;
  } else {
    nextErrors.push(errorMessage(sessionResult.reason, t('ui.overview.errors.streamStatus')));
  }

  if (metadataResult.status === 'fulfilled') {
    hostPlatform.value = metadataResult.value.platform ?? '';
  }
  await refreshVigem(hostPlatform.value || system.metadata?.platform || '');

  fetchErrors.value = [...new Set(nextErrors)];
  lastUpdatedAt.value = Date.now();
  refreshing.value = false;
  loading.value = false;
}

const isStreaming = computed(() =>
  Boolean(session.value?.appRunning || (session.value?.activeSessions ?? 0) > 0),
);

const warnings = computed<OverviewWarning[]>(() => {
  const result: OverviewWarning[] = [];
  if (fetchErrors.value.length) {
    result.push({
      key: 'partial-data',
      title: t('ui.overview.warnings.partialData.title'),
      detail: fetchErrors.value[0],
      to: '/logs',
      action: t('ui.overview.actions.openLogs'),
    });
  }
  if (session.value?.paused) {
    result.push({
      key: 'paused-app',
      title: t('ui.overview.warnings.paused.title'),
      detail: t('ui.overview.warnings.paused.detail', {
        app: session.value.appName || t('ui.overview.currentApplication'),
      }),
      to: '/devices',
      action: t('ui.overview.actions.manageDevices'),
    });
  }
  if (
    session.value?.lastEncoderProbeFailed ||
    system.metadata?.encoder_status?.state === 'failed'
  ) {
    result.push({
      key: 'encoder-probe-failed',
      title: t('ui.overview.warnings.encoderProbe.title'),
      detail: t('ui.overview.warnings.encoderProbe.detail'),
      to: '/logs',
      action: t('ui.overview.actions.openLogs'),
    });
  }
  return result;
});

const showVigemBanner = computed(
  () =>
    isWindowsPlatform(hostPlatform.value || system.metadata?.platform) &&
    controllerEnabled.value === true &&
    vigemInstalled.value === false &&
    vigemRequired.value !== false,
);

const readiness = computed<{ label: string; detail: string; tone: StatusTone }>(() => {
  if (isStreaming.value) {
    return {
      label: t('ui.overview.readiness.streaming'),
      detail: session.value?.appName || t('ui.overview.readiness.remoteSessionActive'),
      tone: 'info',
    };
  }
  if (system.health === 'unknown')
    return {
      label: t('ui.settings.linux.states.unknown'),
      detail: t('ui.settings.linux.unverified'),
      tone: 'neutral',
    };
  if (system.health === 'warning' && !warnings.value.length)
    return {
      label: t('ui.status.needs_attention'),
      detail: t('ui.settings.linux.check_setup'),
      tone: 'warning',
    };
  if (warnings.value.length) {
    return {
      label: t('ui.overview.readiness.attention'),
      detail: warnings.value[0].title,
      tone: 'warning',
    };
  }
  return {
    label: t('ui.overview.readiness.ready'),
    detail: t('ui.overview.readiness.readyDetail'),
    tone: 'success',
  };
});

const lastUpdatedLabel = computed(() =>
  lastUpdatedAt.value
    ? new Intl.DateTimeFormat(locale.value || undefined, {
        hour: 'numeric',
        minute: '2-digit',
      }).format(lastUpdatedAt.value)
    : t('ui.overview.notUpdated'),
);

const readinessIcon = computed(() => {
  if (readiness.value.tone === 'warning') return 'alert-triangle';
  if (readiness.value.tone === 'neutral') return 'info';
  return isStreaming.value ? 'activity' : 'check-circle';
});
const primaryAction = computed(() => {
  if (isStreaming.value)
    return {
      to: '/devices',
      label: t('ui.overview.actions.manageDevices'),
      icon: 'devices',
    } as const;
  if (warnings.value.length)
    return {
      to: warnings.value[0].to,
      label: warnings.value[0].action,
      icon: 'chevron-right',
    } as const;
  if (system.health === 'unknown' || system.health === 'warning')
    return {
      to: '/settings?category=display',
      label: t('ui.overview.actions.reviewSetup'),
      icon: 'settings',
    } as const;
  return { to: '/pair', label: t('ui.overview.actions.pairDevice'), icon: 'plus' } as const;
});
const quickActions = [
  { key: 'library', to: '/library', icon: 'library' },
  { key: 'devices', to: '/devices', icon: 'devices' },
  { key: 'settings', to: '/settings', icon: 'settings' },
] as const;
const stopConfirmDescription = computed(() =>
  (session.value?.activeSessions ?? 0) > 1
    ? t('ui.sessions.confirm.stop_rtsp_all_description')
    : t('ui.sessions.confirm.stop_rtsp_description'),
);

function requestStop(): void {
  stopError.value = '';
  stopNotice.value = '';
  stopConfirmOpen.value = true;
}

async function confirmStop(): Promise<void> {
  stopping.value = true;
  stopError.value = '';
  try {
    const response = await apiPost<MutationResponse>('/api/apps/close', {});
    if (response.status !== true) throw new Error(t('ui.sessions.error.stop_rtsp_rejected'));
    stopNotice.value = t('ui.sessions.notice.stop_rtsp');
    stopConfirmOpen.value = false;
    await refresh(true);
  } catch (cause) {
    stopError.value = errorMessage(cause, t('ui.sessions.error.action'));
  } finally {
    stopping.value = false;
  }
}

function onVisibilityChange(): void {
  if (document.visibilityState === 'visible') void refresh(true);
}

onMounted(() => {
  void refresh();
  pollTimer = window.setInterval(() => {
    if (document.visibilityState === 'visible') void refresh(true);
  }, 10_000);
  document.addEventListener('visibilitychange', onVisibilityChange);
});

onBeforeUnmount(() => {
  if (pollTimer) window.clearInterval(pollTimer);
  document.removeEventListener('visibilitychange', onVisibilityChange);
});
</script>

<template>
  <div class="page page--wide overview-page">
    <PageHeader :title="t('ui.overview.title')" :description="t('ui.overview.description')">
      <template #actions>
        <div class="overview-refresh">
          <span class="overview-updated">{{
            t('ui.overview.updated', { time: lastUpdatedLabel })
          }}</span>
          <AppButton
            icon="refresh"
            :label="t('_common.refresh')"
            variant="secondary"
            :busy="refreshing"
            :busy-label="t('ui.overview.refreshing')"
            @click="refresh(true)"
          />
        </div>
      </template>
    </PageHeader>
    <div class="visually-hidden" aria-live="polite" aria-atomic="true">{{ readiness.label }}</div>
    <template v-if="loading">
      <LoadingSkeleton variant="block" height="184px" :label="t('ui.overview.loadingReadiness')" />
      <LoadingSkeleton variant="block" height="320px" :label="t('ui.overview.loadingReadiness')" />
    </template>
    <template v-else>
      <section
        class="readiness-panel"
        :data-tone="readiness.tone"
        aria-labelledby="readiness-title"
      >
        <div class="readiness-panel__state">
          <span class="readiness-panel__icon" aria-hidden="true"
            ><UiIcon :name="readinessIcon" :size="28"
          /></span>
          <div class="readiness-panel__copy">
            <span class="readiness-panel__eyebrow">{{
              t('ui.overview.readiness.hostStatus')
            }}</span>
            <h2 id="readiness-title">{{ readiness.label }}</h2>
            <p>{{ readiness.detail }}</p>
            <p v-if="isStreaming" class="readiness-panel__sessions">
              {{
                t(
                  (session?.activeSessions ?? 0) === 1
                    ? 'ui.overview.activeSessions.one'
                    : 'ui.overview.activeSessions.other',
                  { count: session?.activeSessions ?? 0 },
                )
              }}
            </p>
          </div>
        </div>
        <div class="readiness-panel__actions">
          <RouterLink class="button button--primary" :to="primaryAction.to">
            <UiIcon :name="primaryAction.icon" />{{ primaryAction.label }}
          </RouterLink>
          <AppButton
            v-if="session?.appRunning"
            icon="stop"
            :label="t('ui.sessions.action.stop_stream')"
            variant="secondary"
            :disabled="stopping"
            @click="requestStop"
          />
        </div>
      </section>

      <InlineAlert v-if="stopError" tone="danger" :title="stopError" announce="assertive" />
      <InlineAlert
        v-if="stopNotice"
        tone="success"
        :title="stopNotice"
        announce="polite"
        :dismiss-label="t('_common.dismiss')"
        @dismiss="stopNotice = ''"
      />
      <div v-if="showVigemBanner || warnings.length" class="overview-notices">
        <InlineAlert v-if="showVigemBanner" tone="warning" :title="t('config.vigem_missing_title')">
          {{ t('config.vigem_missing_desc') }}
          <span v-if="vigemVersion" class="overview-vigem-version">
            ({{ t('config.vigem_detected_version') }}: {{ vigemVersion }})
          </span>
          <template #actions>
            <a
              href="https://github.com/nefarius/ViGEmBus/releases/latest"
              target="_blank"
              rel="noopener noreferrer"
            >
              {{ t('config.vigem_install') }}
              <UiIcon name="external-link" :size="14" aria-hidden="true" />
            </a>
          </template>
        </InlineAlert>
        <InlineAlert
          v-for="warning in warnings"
          :key="warning.key"
          tone="warning"
          :title="warning.title"
        >
          {{ warning.detail }}
          <template #actions
            ><RouterLink :to="warning.to">{{ warning.action }}</RouterLink></template
          >
        </InlineAlert>
      </div>
      <InlineAlert
        v-if="fetchErrors.length > 1"
        tone="warning"
        :title="t('ui.overview.additionalDataUnavailable')"
      >
        {{ fetchErrors.slice(1).join(' ') }}
      </InlineAlert>

      <section class="overview-panel workspace-panel" aria-labelledby="workspace-title">
        <div class="overview-panel__heading">
          <h2 id="workspace-title">{{ t('ui.overview.quickActions.title') }}</h2>
        </div>
        <RouterLink
          v-for="action in quickActions"
          :key="action.key"
          class="workspace-link"
          :to="action.to"
        >
          <span class="workspace-link__icon"><UiIcon :name="action.icon" :size="20" /></span>
          <span class="workspace-link__copy"
            ><strong>{{ t(`ui.overview.quickActions.${action.key}`) }}</strong
            ><span>{{ t(`ui.overview.quickActions.${action.key}Detail`) }}</span></span
          >
          <UiIcon name="chevron-right" :size="16" />
        </RouterLink>
      </section>
      <LinuxCaptureStatus
        v-if="system.metadata?.platform === 'linux' && supportsManagedLinuxDisplay(system.metadata)"
        :metadata="system.metadata"
        :virtual-mode="
          system.metadata.capture_status?.virtual_display_configured === false
            ? 'disabled'
            : undefined
        "
      />
      <footer class="overview-footer">
        <span
          >{{ t('ui.overview.installedVersion') }}
          <strong>{{ system.metadata?.version || t('_common.unknown') }}</strong></span
        >
        <nav :aria-label="t('ui.overview.support')">
          <a
            href="https://github.com/Nonary/Vibepollo/issues/new/choose"
            target="_blank"
            rel="noopener noreferrer"
            >{{ t('ui.overview.actions.reportBug') }}<UiIcon name="external-link" :size="14"
          /></a>
          <a
            href="https://github.com/Nonary/Vibepollo/releases/latest"
            target="_blank"
            rel="noopener noreferrer"
            >{{ t('ui.overview.actions.checkUpdates') }}<UiIcon name="external-link" :size="14"
          /></a>
        </nav>
      </footer>
    </template>
    <ConfirmDialog
      v-model:open="stopConfirmOpen"
      :title="t('ui.sessions.confirm.stop_rtsp_title')"
      :description="stopConfirmDescription"
      :confirm-label="t('ui.sessions.action.stop_stream')"
      :cancel-label="t('_common.cancel')"
      :busy="stopping"
      :busy-label="t('ui.sessions.action.working')"
      :close-on-confirm="false"
      @confirm="confirmStop"
    />
  </div>
</template>

<style scoped>
.overview-page {
  display: grid;
  gap: var(--vs-space-24);
}
.overview-refresh {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--vs-space-16);
}
.overview-updated {
  color: var(--vs-color-text-muted);
  font-size: var(--vs-type-size-metadata);
}
.readiness-panel,
.overview-panel {
  min-width: 0;
  border: var(--vs-border-width) solid var(--vs-color-border-subtle);
  border-radius: var(--vs-radius-card);
  background: var(--vs-color-bg-surface);
}
.readiness-panel {
  --readiness-color: var(--vs-color-status-success);
  display: flex;
  min-height: 184px;
  align-items: center;
  justify-content: space-between;
  gap: var(--vs-space-32);
  padding: var(--vs-space-32);
}
.readiness-panel[data-tone='info'] {
  --readiness-color: var(--vs-color-status-info);
}
.readiness-panel[data-tone='warning'] {
  --readiness-color: var(--vs-color-status-warning);
}
.readiness-panel[data-tone='neutral'] {
  --readiness-color: var(--vs-color-text-muted);
}
.readiness-panel__state {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: var(--vs-space-20);
}
.readiness-panel__copy {
  min-width: 0;
}
.readiness-panel__eyebrow {
  color: var(--readiness-color);
  font-size: var(--vs-type-size-metadata);
  font-weight: var(--vs-type-weight-medium);
}
.readiness-panel__icon {
  display: grid;
  width: var(--vs-space-64);
  height: var(--vs-space-64);
  flex: none;
  place-items: center;
  border: 1px solid color-mix(in srgb, var(--readiness-color) 22%, transparent);
  border-radius: var(--vs-radius-dialog);
  background: color-mix(in srgb, var(--readiness-color) 8%, transparent);
  color: var(--readiness-color);
}
.readiness-panel h2 {
  margin: var(--vs-space-4) 0 var(--vs-space-8);
  font-size: 28px;
  line-height: 36px;
}
.readiness-panel p {
  max-width: 32rem;
  color: var(--vs-color-text-secondary);
  font-size: var(--vs-type-size-control);
}
.readiness-panel__sessions {
  margin-top: var(--vs-space-8);
}
.readiness-panel__actions {
  display: flex;
  flex: 0 0 auto;
  flex-direction: column;
  gap: var(--vs-space-8);
}
.overview-notices {
  display: grid;
  gap: var(--vs-space-12);
}
.overview-vigem-version {
  color: var(--vs-color-text-muted);
}
.overview-panel {
  padding: var(--vs-space-24);
}
.overview-panel__heading {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--vs-space-12);
}
.overview-panel h2 {
  font-size: 16px;
  line-height: 24px;
}
.workspace-panel {
  padding-bottom: var(--vs-space-12);
}
.workspace-panel .overview-panel__heading {
  margin-bottom: var(--vs-space-12);
}
.workspace-link {
  display: flex;
  align-items: center;
  gap: var(--vs-space-16);
  padding: var(--vs-space-16) 0;
  color: var(--vs-color-text-muted);
  text-decoration: none;
}
.workspace-link + .workspace-link {
  border-top: 1px solid var(--vs-color-border-subtle);
}
.workspace-link__icon {
  display: grid;
  flex: none;
  width: 40px;
  height: 40px;
  place-items: center;
  background: var(--vs-color-bg-subtle);
  border-radius: var(--vs-radius-control);
  color: var(--vs-color-text-secondary);
}
.workspace-link__copy {
  min-width: 0;
  flex: 1;
}
.workspace-link strong {
  display: block;
  color: var(--vs-color-text-primary);
  font-size: var(--vs-type-size-control);
  font-weight: var(--vs-type-weight-medium);
}
.workspace-link__copy > span {
  display: block;
  margin-top: var(--vs-space-4);
  font-size: var(--vs-type-size-metadata);
  line-height: 20px;
}
.workspace-link:hover strong,
.workspace-link:hover > svg,
.workspace-link:hover .workspace-link__icon {
  color: var(--vs-color-accent-default);
}
.overview-page :deep(.linux-capture) {
  margin-bottom: 0;
}
.overview-footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--vs-space-16);
  color: var(--vs-color-text-muted);
  font-size: var(--vs-type-size-helper);
}
.overview-footer strong {
  margin-left: var(--vs-space-8);
  font-weight: var(--vs-type-weight-medium);
  color: var(--vs-color-text-secondary);
}
.overview-footer nav {
  display: flex;
  flex-wrap: wrap;
  gap: var(--vs-space-24);
}
.overview-footer a {
  display: inline-flex;
  align-items: center;
  gap: var(--vs-space-8);
  color: var(--vs-color-text-muted);
  text-decoration: none;
}
.overview-footer a:hover {
  color: var(--vs-color-accent-default);
}
@media (max-width: 767px) {
  .readiness-panel {
    padding: var(--vs-space-24);
    align-items: stretch;
    flex-direction: column;
    gap: var(--vs-space-24);
  }
  .readiness-panel__state {
    align-items: flex-start;
  }
  .readiness-panel__icon {
    width: 44px;
    height: 44px;
  }
  .readiness-panel h2 {
    font-size: 24px;
    line-height: 32px;
  }
  .overview-panel {
    padding: var(--vs-space-20);
  }
}
@media (max-width: 359px) {
  .readiness-panel__state {
    flex-direction: column;
  }
}
@media (forced-colors: active) {
  .readiness-panel__icon {
    border: 1px solid CanvasText;
  }
}
</style>

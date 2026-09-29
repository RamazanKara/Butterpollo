<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { useI18n } from 'vue-i18n';

import { ApiError, apiGet, apiPost } from '@/api/client';
import {
  AppButton,
  ConfirmDialog,
  InlineAlert,
  LoadingSkeleton,
  PageHeader,
  StatusBadge,
  UiIcon,
  type StatusTone,
} from '@/components/ui';
import { useSystemStore } from '@/stores/system';

interface RtssStatus {
  enabled?: boolean;
  configured_provider?: string;
  active_provider?: string;
  path_exists?: boolean;
  hooks_found?: boolean;
  profile_found?: boolean;
  process_running?: boolean;
  message?: string;
}

interface VigemStatus {
  installed?: boolean;
  version?: string;
  version_compatible?: boolean;
  packaged_version?: string;
  error?: string;
  // False when Vibepollo's own virtual gamepad driver is available, so a missing
  // ViGEmBus is an unused option rather than a problem.
  required?: boolean;
}

interface VulkanStatus {
  installed?: boolean;
  enabled?: boolean;
}

interface MutationResult {
  status?: boolean;
  error?: string;
}

type IntegrationId = 'rtss' | 'vigem' | 'vulkan';
type PendingAction = 'vigem-install' | 'vulkan-register';

interface IntegrationSummary {
  id: IntegrationId;
  name: string;
  description: string;
  status: string;
  tone: StatusTone;
  details: string[];
}

const { t } = useI18n();
const system = useSystemStore();
const rtss = ref<RtssStatus | null>(null);
const vigem = ref<VigemStatus | null>(null);
const vulkan = ref<VulkanStatus | null>(null);
const loading = ref(true);
const refreshing = ref(false);
const actionBusy = ref(false);
const errors = ref<Partial<Record<IntegrationId, string>>>({});
const notice = ref('');
const confirmOpen = ref(false);
const pendingAction = ref<PendingAction | null>(null);

const isWindows = computed(() =>
  String(system.metadata?.platform ?? '')
    .toLocaleLowerCase()
    .includes('windows'),
);

function message(cause: unknown, fallback: string): string {
  return cause instanceof ApiError ? fallback : cause instanceof Error ? cause.message : fallback;
}

async function load(preserveNotice = false): Promise<void> {
  if (refreshing.value) return;
  refreshing.value = true;
  if (!preserveNotice) notice.value = '';
  const nextErrors: Partial<Record<IntegrationId, string>> = {};

  if (!system.metadata) await system.refreshHost();

  const windowsResults = isWindows.value
    ? await Promise.allSettled([
        apiGet<RtssStatus>('/api/rtss/status'),
        apiGet<VigemStatus>('/api/vigembus/status'),
        apiGet<VulkanStatus>('/api/health/vulkan-hdr-layer'),
      ])
    : [];
  const [rtssResult, vigemResult, vulkanResult] = windowsResults;

  if (rtssResult?.status === 'fulfilled') rtss.value = rtssResult.value;
  else if (rtssResult)
    nextErrors.rtss = message(rtssResult.reason, t('ui.integrations.errors.rtssStatus'));

  if (vigemResult?.status === 'fulfilled') vigem.value = vigemResult.value;
  else if (vigemResult)
    nextErrors.vigem = message(vigemResult.reason, t('ui.integrations.errors.vigemStatus'));

  if (vulkanResult?.status === 'fulfilled') vulkan.value = vulkanResult.value;
  else if (vulkanResult)
    nextErrors.vulkan = message(vulkanResult.reason, t('ui.integrations.errors.vulkanStatus'));

  errors.value = nextErrors;
  loading.value = false;
  refreshing.value = false;
}

function rtssSummary(): IntegrationSummary {
  const value = rtss.value;
  const name = t('ui.integrations.rtss.name');
  const shortDescription = t('ui.integrations.rtss.shortDescription');
  if (!isWindows.value) return unavailableSummary('rtss', name, shortDescription);
  if (!value) return failedSummary('rtss', name, shortDescription);
  const ready = Boolean(value.path_exists && value.hooks_found);
  const status =
    value.active_provider === 'rtss'
      ? t('_common.active')
      : ready
        ? t('ui.integrations.status.ready')
        : value.path_exists
          ? t('ui.integrations.status.repairNeeded')
          : t('ui.integrations.status.notDetected');
  const tone: StatusTone =
    value.active_provider === 'rtss' || ready ? 'success' : value.enabled ? 'warning' : 'neutral';
  return {
    id: 'rtss',
    name,
    description: t('ui.integrations.rtss.description'),
    status,
    tone,
    details: [
      value.configured_provider
        ? t('rtss.status_configured_provider', { provider: value.configured_provider })
        : '',
      value.process_running ? t('ui.integrations.rtss.processRunning') : '',
      value.message || '',
    ].filter(Boolean),
  };
}

function vigemSummary(): IntegrationSummary {
  const value = vigem.value;
  const name = t('ui.integrations.vigem.name');
  const shortDescription = t('ui.integrations.vigem.shortDescription');
  if (!isWindows.value) return unavailableSummary('vigem', name, shortDescription);
  if (!value) return failedSummary('vigem', name, shortDescription);
  const compatible = Boolean(value.installed && value.version_compatible);
  // The server reports required=false when another driver is already providing
  // controllers. Flagging ViGEmBus in that case sends people off installing a
  // dependency the host does not use.
  const required = value.required !== false;
  const notNeeded = !compatible && !required;
  return {
    id: 'vigem',
    name,
    description: notNeeded
      ? t('ui.integrations.vigem.supersededDescription')
      : t('ui.integrations.vigem.description'),
    status: compatible
      ? t('ui.integrations.status.ready')
      : notNeeded
        ? t('ui.integrations.status.notRequired')
        : value.installed
          ? t('ui.integrations.status.updateRequired')
          : t('ui.integrations.status.notInstalled'),
    tone: compatible ? 'success' : notNeeded ? 'neutral' : 'warning',
    details: [
      value.version
        ? t('ui.integrations.details.installedVersion', { version: value.version })
        : '',
      value.packaged_version
        ? t('ui.integrations.details.bundledVersion', { version: value.packaged_version })
        : '',
      value.error || '',
    ].filter(Boolean),
  };
}

function vulkanSummary(): IntegrationSummary {
  const value = vulkan.value;
  const name = t('vulkan_hdr.troubleshooting_title');
  const shortDescription = t('ui.integrations.vulkan.shortDescription');
  if (!isWindows.value) return unavailableSummary('vulkan', name, shortDescription);
  if (!value) return failedSummary('vulkan', name, shortDescription);
  const healthy = Boolean(value.installed && value.enabled);
  return {
    id: 'vulkan',
    name,
    description: t('ui.integrations.vulkan.description'),
    status: healthy
      ? t('ui.integrations.vulkan.enabledAndRegistered')
      : value.enabled
        ? t('ui.integrations.vulkan.registrationMissing')
        : value.installed
          ? t('ui.integrations.vulkan.registeredDisabled')
          : t('_common.disabled'),
    tone: healthy ? 'success' : value.enabled && !value.installed ? 'warning' : 'neutral',
    details: [
      value.enabled
        ? t('ui.integrations.vulkan.enabledInConfiguration')
        : t('ui.integrations.vulkan.disabledInConfiguration'),
    ],
  };
}

function unavailableSummary(
  id: IntegrationId,
  name: string,
  description: string,
): IntegrationSummary {
  return {
    id,
    name,
    description,
    status: t('ui.integrations.status.windowsOnly'),
    tone: 'neutral',
    details: [],
  };
}

function failedSummary(id: IntegrationId, name: string, description: string): IntegrationSummary {
  return {
    id,
    name,
    description,
    status: t('ui.integrations.status.unavailable'),
    tone: 'danger',
    details: [errors.value[id] || t('ui.integrations.errors.statusLoad')],
  };
}

const summaries = computed(() =>
  isWindows.value ? [rtssSummary(), vigemSummary(), vulkanSummary()] : [],
);

const errorCount = computed(() => Object.keys(errors.value).length);

const dialogCopy = computed(() => {
  switch (pendingAction.value) {
    case 'vigem-install':
      return {
        title: vigem.value?.installed
          ? t('ui.integrations.confirm.vigemRepairTitle')
          : t('ui.integrations.confirm.vigemInstallTitle'),
        description: t('ui.integrations.confirm.vigemDescription'),
        confirm: vigem.value?.installed
          ? t('ui.integrations.actions.repairDriver')
          : t('ui.integrations.actions.installDriver'),
        tone: 'default' as const,
      };
    case 'vulkan-register':
      return {
        title: t('ui.integrations.confirm.vulkanRegisterTitle'),
        description: t('ui.integrations.confirm.vulkanRegisterDescription'),
        confirm: t('ui.integrations.actions.registerLayer'),
        tone: 'default' as const,
      };
    default:
      return {
        title: t('ui.integrations.confirm.defaultTitle'),
        description: '',
        confirm: t('_common.continue'),
        tone: 'default' as const,
      };
  }
});

function requestAction(action: PendingAction): void {
  pendingAction.value = action;
  confirmOpen.value = true;
}

function updateConfirmOpen(value: boolean): void {
  confirmOpen.value = value;
  if (!value && !actionBusy.value) pendingAction.value = null;
}

async function runConfirmedAction(): Promise<void> {
  const action = pendingAction.value;
  if (!action || actionBusy.value) return;
  actionBusy.value = true;
  notice.value = '';
  try {
    let result: MutationResult;
    if (action === 'vigem-install') {
      result = await apiPost<MutationResult>('/api/vigembus/install', {});
      notice.value = t('ui.integrations.notices.vigemCompleted');
    } else {
      result = await apiPost<MutationResult>('/api/health/vulkan-hdr-layer/register', {});
      notice.value = t('ui.integrations.notices.vulkanRefreshed');
    }
    if (result.status === false) {
      throw new Error(result.error || t('ui.integrations.errors.actionIncomplete'));
    }
    await load(true);
  } catch (cause) {
    notice.value = '';
    errors.value = {
      ...errors.value,
      [action === 'vigem-install' ? 'vigem' : 'vulkan']: message(
        cause,
        t('ui.integrations.errors.actionFailed'),
      ),
    };
  } finally {
    actionBusy.value = false;
    pendingAction.value = null;
  }
}

const route = useRoute();
watch(
  () => [loading.value, route.hash],
  async () => {
    if (loading.value || !route.hash) return;
    await nextTick();
    document.getElementById(route.hash.slice(1))?.scrollIntoView({ block: 'start' });
  },
);
onMounted(() => void load());
</script>

<template>
  <div class="page page--narrow integrations-page">
    <PageHeader :title="t('ui.integrations.title')" :description="t('ui.integrations.description')">
      <template #actions>
        <AppButton
          icon="refresh"
          :label="t('ui.integrations.actions.refreshStatus')"
          variant="secondary"
          :busy="refreshing"
          :busy-label="t('ui.integrations.refreshing')"
          @click="load()"
        />
      </template>
    </PageHeader>

    <InlineAlert
      v-if="notice"
      tone="success"
      :title="t('ui.integrations.updated')"
      announce="polite"
    >
      {{ notice }}
    </InlineAlert>
    <InlineAlert v-if="errorCount" tone="warning" :title="t('ui.integrations.checksFailed')">
      {{
        t(
          errorCount === 1
            ? 'ui.integrations.failedCount.one'
            : 'ui.integrations.failedCount.other',
          { count: errorCount },
        )
      }}
    </InlineAlert>

    <div v-if="loading" class="integration-list" :aria-label="t('ui.integrations.loadingStatus')">
      <LoadingSkeleton v-for="index in 3" :key="index" variant="block" height="112px" />
    </div>

    <section
      v-else
      class="integration-list"
      :aria-label="t('ui.integrations.installedIntegrations')"
    >
      <article
        v-for="summary in summaries"
        :key="summary.id"
        class="integration-row"
        :aria-labelledby="`integration-${summary.id}`"
      >
        <span class="integration-row__icon" aria-hidden="true">
          <UiIcon :name="summary.id === 'vigem' ? 'gamepad' : 'integrations'" :size="20" />
        </span>
        <div class="integration-row__main">
          <div class="integration-row__title">
            <h2 :id="`integration-${summary.id}`">{{ summary.name }}</h2>
            <StatusBadge :label="summary.status" :tone="summary.tone" compact />
          </div>
          <ul v-if="summary.details.length" class="integration-details">
            <li v-for="detail in summary.details" :key="detail">{{ detail }}</li>
          </ul>
        </div>
        <div class="integration-row__actions">
          <AppButton
            v-if="
              summary.id === 'vigem' &&
              vigem?.required !== false &&
              (!vigem?.installed || !vigem?.version_compatible)
            "
            :label="
              vigem?.installed
                ? t('ui.integrations.actions.repair')
                : t('ui.integrations.actions.install')
            "
            variant="secondary"
            size="compact"
            @click="requestAction('vigem-install')"
          />
          <AppButton
            v-else-if="summary.id === 'vulkan' && vulkan?.enabled && !vulkan?.installed"
            :label="t('ui.integrations.actions.repairRegistration')"
            variant="secondary"
            size="compact"
            @click="requestAction('vulkan-register')"
          />
        </div>
      </article>
    </section>

    <ConfirmDialog
      :open="confirmOpen"
      :title="dialogCopy.title"
      :description="dialogCopy.description"
      :confirm-label="dialogCopy.confirm"
      :tone="dialogCopy.tone"
      :busy="actionBusy"
      :busy-label="t('ui.integrations.applying')"
      @update:open="updateConfirmOpen"
      @confirm="runConfirmedAction"
    />
  </div>
</template>

<style scoped>
.integrations-page {
  display: grid;
  gap: var(--vs-space-20);
}

.integration-list {
  display: grid;
  overflow: hidden;
  border: var(--vs-border-width) solid var(--vs-color-border-subtle);
  border-radius: var(--vs-radius-card);
  background: var(--vs-color-bg-surface);
}

.integration-row {
  display: grid;
  min-height: 7rem;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: start;
  gap: var(--vs-space-16);
  padding: var(--vs-space-16) var(--vs-space-20);
}

.integration-row + .integration-row {
  border-top: var(--vs-border-width) solid var(--vs-color-border-subtle);
}

.integration-row__icon {
  display: grid;
  width: 2.5rem;
  height: 2.5rem;
  place-items: center;
  border-radius: var(--vs-radius-control);
  background: var(--vs-color-bg-subtle);
  color: var(--vs-color-text-secondary);
}

.integration-row__main {
  display: grid;
  min-width: 0;
  gap: var(--vs-space-4);
}

.integration-row__title {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--vs-space-8);
}

.integration-row h2 {
  font-size: var(--vs-type-size-section);
  line-height: var(--vs-type-line-height-section);
}

.integration-row p,
.integration-details {
  color: var(--vs-color-text-secondary);
  font-size: var(--vs-type-size-metadata);
  line-height: var(--vs-type-line-height-metadata);
}

.integration-details {
  display: flex;
  min-width: 0;
  flex-wrap: wrap;
  gap: var(--vs-space-4) var(--vs-space-16);
  padding: 0;
  margin-top: var(--vs-space-4);
  list-style: none;
}

.integration-details li {
  overflow-wrap: anywhere;
}

.integration-row__actions {
  display: flex;
  max-width: 18rem;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--vs-space-4);
}

@media (max-width: 47.999rem) {
  .integration-row {
    grid-template-columns: auto minmax(0, 1fr);
    padding-inline: var(--vs-space-16);
  }

  .integration-row__actions {
    grid-column: 1 / -1;
    max-width: none;
    justify-content: flex-start;
    padding-inline-start: calc(2.5rem + var(--vs-space-16));
  }
}

@media (max-width: 29.999rem) {
  .integration-row {
    grid-template-columns: minmax(0, 1fr);
  }

  .integration-row__icon {
    display: none;
  }

  .integration-row__actions {
    padding-inline-start: 0;
  }
}

@media (forced-colors: active) {
  .integration-list,
  .integration-row__icon {
    border: var(--vs-border-width) solid CanvasText;
  }
}
</style>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';

import { apiGet } from '@/api/client';
import { AppButton, StatusBadge } from '@/components/ui';

type StatusTone = 'neutral' | 'info' | 'success' | 'warning' | 'danger';

interface RtssStatus {
  active_provider?: string;
  configured_provider?: string;
  configured_path?: string;
  path_configured?: boolean;
  resolved_path?: string;
  path_exists?: boolean;
  hooks_found?: boolean;
  process_running?: boolean;
  enabled?: boolean;
}

const props = withDefaults(
  defineProps<{
    inputId: string;
    modelValue?: unknown;
  }>(),
  {
    modelValue: () => '',
  },
);

const emit = defineEmits<{
  'update:modelValue': [value: string];
}>();

const { t } = useI18n();

const status = ref<RtssStatus | null>(null);
const loading = ref(false);
const loadError = ref(false);
const userEdited = ref(false);
const draftPath = ref(normalizeWindowsPath(props.modelValue));

function normalizeWindowsPath(raw: unknown): string {
  if (raw === null || raw === undefined) return '';
  let value = String(raw).replace(/\//g, '\\').trim();
  if (!value) return '';

  let prefix = '';
  if (value.startsWith('\\\\?\\')) {
    prefix = '\\\\?\\';
    value = value.slice(4);
  } else if (value.startsWith('\\\\')) {
    prefix = '\\\\';
    value = value.slice(2);
  }

  value = value.replace(/\\{2,}/g, '\\');
  if (prefix === '\\\\' && value.startsWith('\\')) {
    value = value.slice(1);
  }
  return prefix + value;
}

const configuredPath = computed(() => normalizeWindowsPath(props.modelValue));

const detectedPath = computed(() =>
  status.value?.path_exists ? normalizeWindowsPath(status.value.resolved_path) : '',
);

const ready = computed(() => Boolean(status.value?.path_exists && status.value.hooks_found));

const needsAttention = computed(() =>
  Boolean(status.value?.path_exists && !status.value.hooks_found),
);

const notFound = computed(() =>
  Boolean(status.value?.path_configured && !status.value.path_exists),
);

const statusTone = computed<StatusTone>(() => {
  if (loading.value) return 'neutral';
  if (loadError.value) return 'warning';
  if (ready.value) return 'success';
  if (notFound.value) return 'danger';
  if (needsAttention.value) return 'warning';
  return 'neutral';
});

const statusLabel = computed(() => {
  if (loading.value) return t('ui.settings.integrations.checking');
  if (loadError.value) return t('ui.settings.integrations.status_unavailable');
  if (ready.value) return t('ui.settings.integrations.ready');
  if (notFound.value) return t('ui.settings.integrations.not_found');
  if (needsAttention.value) return t('ui.settings.integrations.needs_attention');
  return t('ui.settings.integrations.not_detected');
});

const statusDescription = computed(() => {
  if (loading.value) return t('ui.settings.integrations.checking_description');
  if (loadError.value) return t('ui.settings.integrations.status_unavailable_description');
  if (ready.value) {
    return status.value?.active_provider === 'rtss'
      ? t('ui.settings.integrations.rtss.active')
      : status.value?.configured_provider === 'rtss'
        ? t('ui.settings.integrations.rtss.selected')
        : status.value?.enabled === false
          ? t('ui.settings.integrations.rtss.disabled')
          : status.value?.configured_provider === 'auto'
            ? t('ui.settings.integrations.rtss.auto')
            : t('ui.settings.integrations.rtss.ready');
  }
  if (needsAttention.value) return t('ui.settings.integrations.rtss.hooks_missing');
  if (notFound.value) return t('ui.settings.integrations.rtss.path_missing');
  return t('ui.settings.integrations.rtss.not_detected');
});

const automaticHintVisible = computed(() => Boolean(detectedPath.value) && !configuredPath.value);

const canSaveDetectedPath = computed(
  () => Boolean(detectedPath.value) && detectedPath.value !== configuredPath.value,
);

function syncDetectedPath(): void {
  if (!userEdited.value && !configuredPath.value && detectedPath.value) {
    draftPath.value = detectedPath.value;
  }
}

async function refresh(): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  loadError.value = false;

  try {
    status.value = await apiGet<RtssStatus>('/api/rtss/status');
    syncDetectedPath();
  } catch {
    status.value = null;
    loadError.value = true;
  } finally {
    loading.value = false;
  }
}

function updatePath(value: string): void {
  userEdited.value = true;
  draftPath.value = normalizeWindowsPath(value);
  emit('update:modelValue', draftPath.value);
}

function useDetectedPath(): void {
  const path = detectedPath.value;
  if (!path) return;
  updatePath(path);
}

watch(
  () => props.modelValue,
  (value) => {
    const next = normalizeWindowsPath(value);
    if (next === draftPath.value) return;
    draftPath.value = next;
    userEdited.value = false;
  },
);

onMounted(() => void refresh());
</script>

<template>
  <div class="integration-path">
    <div class="integration-path__status">
      <div class="integration-path__status-copy">
        <StatusBadge :tone="statusTone" :label="statusLabel" compact />
        <p>{{ statusDescription }}</p>
      </div>
      <div class="integration-path__actions">
        <AppButton
          v-if="canSaveDetectedPath"
          variant="tertiary"
          size="compact"
          icon="check"
          :label="t('ui.settings.integrations.use_detected')"
          @click="useDetectedPath"
        />
        <AppButton
          variant="tertiary"
          size="compact"
          icon="refresh"
          :label="t('ui.settings.integrations.rescan')"
          :busy="loading"
          :busy-label="t('ui.settings.integrations.checking')"
          @click="refresh"
        />
      </div>
    </div>

    <div class="integration-path__input-row">
      <input
        :id="inputId"
        class="vs-input monospace"
        type="text"
        :value="draftPath"
        :aria-describedby="`${inputId}-status`"
        @input="updatePath(($event.target as HTMLInputElement).value)"
      />
    </div>

    <p v-if="automaticHintVisible" class="integration-path__hint" :id="`${inputId}-status`">
      {{ t('ui.settings.integrations.automatic_path_hint') }}
    </p>
    <p v-else class="integration-path__hint" :id="`${inputId}-status`">
      {{ t('ui.settings.integrations.explicit_path_hint') }}
    </p>
  </div>
</template>

<style scoped>
.integration-path {
  display: grid;
  gap: var(--vs-space-8);
}

.integration-path__status,
.integration-path__status-copy,
.integration-path__actions {
  display: flex;
}

.integration-path__status {
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--vs-space-12);
}

.integration-path__status-copy {
  min-width: 0;
  flex: 1;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--vs-space-8);
}

.integration-path__status-copy p {
  flex: 1 1 100%;
  margin: 0;
  color: var(--vs-color-text-secondary);
  font-size: 13px;
  line-height: 18px;
}

.integration-path__actions {
  flex: 0 0 auto;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--vs-space-4);
}

.integration-path__input-row {
  min-width: 0;
}

.integration-path__input-row .vs-input {
  width: 100%;
}

.integration-path__hint {
  margin: 0;
  color: var(--vs-color-text-muted);
  font-size: 12px;
  line-height: 17px;
}

@media (max-width: 767px) {
  .integration-path__status {
    flex-direction: column;
  }

  .integration-path__actions {
    justify-content: flex-start;
  }
}
</style>

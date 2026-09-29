<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { NButton, NAlert, NTag, NSelect } from 'naive-ui';
import type { FrameGenHealth, FrameGenRequirementStatus, FrameGenerationMode } from './types';
import { frameGenDisplayNotice } from './frameGenDisplayPolicy';

const { t } = useI18n();

const modeModel = defineModel<FrameGenerationMode>('mode', { default: 'off' });

const props = defineProps<{
  health: FrameGenHealth | null;
  healthLoading: boolean;
  healthError: string | null;
  nvidiaActive: boolean;
  usingVirtualDisplay: boolean;
}>();

const emit = defineEmits<{
  (e: 'refresh-health'): void;
  (e: 'enable-virtual-screen'): void;
}>();

const hasHealthData = computed(() => !!props.health);
const frameGenOptions = computed(() => [
  { label: t('apps.framegen.mode_none'), value: 'off' as const },
  { label: t('apps.framegen.provider_game_provided'), value: 'game-provided' as const },
  { label: 'NVIDIA Smooth Motion', value: 'nvidia-smooth-motion' as const },
]);
const frameGenDisplayAlert = computed(() => {
  const notice = frameGenDisplayNotice(props.usingVirtualDisplay, modeModel.value);
  return notice ? { type: notice.type, message: t(notice.key) } : null;
});
const requirementRows = computed(() => {
  if (!props.health) return [];
  return [
    {
      id: 'os',
      icon: 'fab fa-windows',
      label: t('apps.framegen.req_os_label'),
      status: props.health.os.status,
      message: props.health.os.message,
    },
    {
      id: 'capture',
      icon: 'fas fa-desktop',
      label: t('apps.framegen.req_capture_label'),
      status: props.health.capture.status,
      message: props.health.capture.message,
    },
    {
      id: 'rtss',
      icon: 'fas fa-stopwatch-20',
      label: t('apps.framegen.req_rtss_label'),
      status: props.health.rtss.status,
      message: props.health.rtss.message,
    },
    {
      id: 'display',
      icon: 'fas fa-tv',
      label: props.usingVirtualDisplay
        ? t('apps.framegen.req_display_virtual_label')
        : t('apps.framegen.req_display_physical_label'),
      status: props.health.display.status,
      message: props.health.display.message,
    },
  ];
});

function statusClasses(status: FrameGenRequirementStatus) {
  switch (status) {
    case 'pass':
      return 'bg-emerald-500/10 text-emerald-500';
    case 'warn':
      return 'bg-amber-500/10 text-amber-500';
    case 'fail':
      return 'bg-rose-500/10 text-rose-500';
    default:
      return 'bg-slate-500/10 text-slate-400';
  }
}

function statusIcon(status: FrameGenRequirementStatus) {
  switch (status) {
    case 'pass':
      return 'fas fa-check-circle';
    case 'warn':
      return 'fas fa-exclamation-triangle';
    case 'fail':
      return 'fas fa-times-circle';
    default:
      return 'fas fa-question-circle';
  }
}

function statusLabel(status: FrameGenRequirementStatus) {
  switch (status) {
    case 'pass':
      return t('apps.framegen.status_ready');
    case 'warn':
      return t('apps.framegen.status_warn');
    case 'fail':
      return t('apps.framegen.status_fail');
    default:
      return t('apps.framegen.status_unknown');
  }
}

function targetIconClass(supported: boolean | null) {
  if (supported === true) return 'fas fa-check-circle text-emerald-500';
  if (supported === false) return 'fas fa-times-circle text-rose-500';
  return 'fas fa-question-circle text-amber-500';
}

function targetStatusLabel(supported: boolean | null) {
  if (supported === true) return t('apps.framegen.target_supported');
  if (supported === false) return t('apps.framegen.target_unsupported');
  return t('apps.framegen.target_unknown');
}

const showSuggestion = computed(() => {
  const health = props.health;
  if (!health || !health.suggestion) return null;
  return health.suggestion;
});
const canEnableVirtualScreen = computed(() => !props.usingVirtualDisplay);

const displayTargets = computed(() => props.health?.display.targets || []);
</script>

<template>
  <section
    class="rounded-2xl border border-dark/10 dark:border-light/10 bg-light/60 dark:bg-surface/40 p-4 space-y-4"
  >
    <div class="flex flex-col gap-3 md:flex-row md:items-start md:justify-between">
      <div class="space-y-1">
        <h3 class="text-base font-semibold text-dark dark:text-light">
          {{ $t('apps.framegen.title') }}
        </h3>
        <p class="text-[12px] leading-relaxed opacity-70">
          {{ $t('apps.framegen.subtitle') }}
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <n-tag v-if="nvidiaActive" size="small" type="info">
            <i class="fab fa-nvidia mr-1" /> {{ $t('apps.framegen.tag_nvidia_active') }}
          </n-tag>
          <n-tag v-if="usingVirtualDisplay" size="small" type="success">
            <i class="fas fa-display mr-1" /> {{ $t('apps.framegen.tag_virtual_active') }}
          </n-tag>
        </div>
      </div>
      <div class="flex items-center gap-2">
        <n-button size="small" tertiary :loading="healthLoading" @click="emit('refresh-health')">
          <i class="fas fa-stethoscope" />
          <span class="ml-2">{{ $t('apps.framegen.run_health_check') }}</span>
        </n-button>
      </div>
    </div>

    <n-alert
      v-if="frameGenDisplayAlert"
      :type="frameGenDisplayAlert.type"
      size="small"
      :bordered="false"
    >
      <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <span>{{ frameGenDisplayAlert.message }}</span>
        <n-button
          v-if="canEnableVirtualScreen"
          size="small"
          type="primary"
          @click="emit('enable-virtual-screen')"
        >
          {{ $t('apps.framegen.use_virtual_display') }}
        </n-button>
      </div>
    </n-alert>

    <div class="space-y-4">
      <div class="space-y-2">
        <label class="text-xs font-semibold uppercase tracking-wide opacity-70">
          {{ $t('apps.framegen.kind_label') }}
        </label>
        <n-select
          v-model:value="modeModel"
          :options="frameGenOptions"
          size="small"
          :clearable="false"
        />
        <p class="text-[12px] opacity-70 leading-relaxed">
          {{ $t('apps.framegen.kind_hint') }}
        </p>
      </div>

      <div class="space-y-3">
        <n-alert v-if="healthError" type="error" size="small">
          {{ healthError }}
        </n-alert>
        <n-alert v-else-if="!hasHealthData && !healthLoading" size="small" type="info">
          {{ $t('apps.framegen.health_prompt') }}
        </n-alert>
        <n-alert
          v-else-if="healthLoading && !hasHealthData"
          type="info"
          size="small"
          :bordered="false"
        >
          {{ $t('apps.framegen.health_checking') }}
        </n-alert>
      </div>

      <div v-if="health" class="space-y-3">
        <div
          v-for="row in requirementRows"
          :key="row.id"
          class="rounded-xl border border-dark/10 dark:border-light/10 bg-white/40 dark:bg-white/5 p-3"
        >
          <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div class="flex items-start gap-3">
              <div class="text-primary text-base">
                <i :class="row.icon" />
              </div>
              <div class="space-y-1">
                <div class="font-medium text-sm">{{ row.label }}</div>
                <p class="text-[12px] opacity-70 leading-relaxed">
                  {{ row.message }}
                </p>
              </div>
            </div>
            <div
              :class="[
                'inline-flex items-center gap-1 rounded-full px-2 py-1 text-[12px] font-semibold whitespace-nowrap',
                statusClasses(row.status),
              ]"
            >
              <i :class="statusIcon(row.status)" />
              <span>{{ statusLabel(row.status) }}</span>
            </div>
          </div>
        </div>

        <div
          class="rounded-xl border border-dark/10 dark:border-light/10 bg-white/40 dark:bg-white/5 p-3 space-y-3"
        >
          <div class="space-y-1">
            <div class="flex flex-col gap-1 sm:flex-row sm:items-center sm:justify-between">
              <div class="font-medium text-sm">
                {{ $t('apps.framegen.refresh_coverage_title') }}
              </div>
              <div class="text-[12px] opacity-70">
                {{
                  $t('apps.framegen.targeted_display_label', {
                    label: health.display.deviceLabel || $t('apps.framegen.targeted_display'),
                  })
                }}
              </div>
            </div>
            <p class="text-[12px] opacity-70 leading-relaxed">
              {{ health.display.message }}
            </p>
          </div>

          <div class="grid gap-2 sm:grid-cols-2">
            <div
              v-for="target in displayTargets"
              :key="target.fps"
              class="rounded-lg border border-dark/10 dark:border-light/10 bg-white/50 dark:bg-white/10 px-3 py-2 space-y-1"
            >
              <div class="flex items-center gap-2 text-sm font-medium">
                <i :class="targetIconClass(target.supported)" />
                <span>{{ $t('apps.framegen.fps_stream', { fps: target.fps }) }}</span>
              </div>
              <div class="text-[12px] opacity-70 leading-relaxed">
                {{
                  $t('apps.framegen.needs_hz', {
                    hz: target.requiredHz,
                    status: targetStatusLabel(target.supported),
                  })
                }}
              </div>
            </div>
          </div>

          <n-alert
            v-if="health.display.error"
            type="warning"
            size="small"
            :show-icon="false"
            class="text-[12px]"
          >
            {{ health.display.error }}
          </n-alert>
        </div>
      </div>

      <n-alert
        v-if="showSuggestion"
        :type="showSuggestion.emphasis === 'warning' ? 'warning' : 'info'"
        size="small"
      >
        <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <span>{{ showSuggestion.message }}</span>
          <n-button
            v-if="canEnableVirtualScreen"
            size="small"
            type="primary"
            @click="emit('enable-virtual-screen')"
          >
            {{ $t('apps.framegen.use_virtual_display') }}
          </n-button>
        </div>
      </n-alert>

      <p class="text-[12px] opacity-70 leading-relaxed">
        {{ $t('apps.framegen.pacing_note') }}
      </p>
    </div>
  </section>
</template>

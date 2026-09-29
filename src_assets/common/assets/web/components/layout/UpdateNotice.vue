<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { apiGet } from '@/services/api';
import { AppButton, InlineAlert } from '@/components/ui';
import { checkForUpdates, fetchUpdateStatus } from '@/services/updates';
import { useSystemStore } from '@/stores/system';
import type { ChangelogEntry } from '@/utils/changelog';
import { releasePageUrl, selectAvailableUpdate } from '@/utils/updates';

// The host checks GitHub on its own schedule; this only re-reads its result.
const REFRESH_MS = 300000;

const system = useSystemStore();
const { t } = useI18n();
const available = ref<ChangelogEntry | null>(null);
const failed = ref(false);
const loading = ref(false);
let timer: ReturnType<typeof setTimeout> | undefined;
let stopped = false;
let controller: AbortController | undefined;

async function check(now = false): Promise<void> {
  if (loading.value || !system.metadata?.version || stopped) return;
  clearTimeout(timer);
  loading.value = true;
  controller = new AbortController();
  const signal = controller.signal;
  const timeout = setTimeout(() => controller?.abort(), now ? 60000 : 15000);
  try {
    const [config, status] = await Promise.all([
      apiGet<Record<string, unknown>>('/api/config', { signal }),
      now ? checkForUpdates(signal) : fetchUpdateStatus(signal),
    ]);
    available.value = selectAvailableUpdate(
      system.metadata.version,
      status.releases,
      config.notify_pre_releases,
    );
    failed.value = status.checkFailed;
  } catch {
    if (!stopped) failed.value = true;
  } finally {
    clearTimeout(timeout);
    loading.value = false;
    if (!stopped) timer = setTimeout(() => void check(), REFRESH_MS);
  }
}

watch(
  () => system.metadata?.version,
  () => void check(),
  { immediate: true },
);
onBeforeUnmount(() => {
  stopped = true;
  clearTimeout(timer);
  controller?.abort();
});
</script>

<template>
  <div v-if="available || failed" class="update-notice">
    <InlineAlert
      v-if="available"
      tone="info"
      announce="polite"
      :title="t('ui.updates.available', { version: available.tag })"
    >
      <a :href="releasePageUrl(available)" target="_blank" rel="noopener noreferrer">{{
        t('ui.maintenance.releases.open')
      }}</a>
    </InlineAlert>
    <InlineAlert v-if="failed" tone="warning" :title="t('ui.maintenance.releases.unavailable')">
      <AppButton
        :label="t('ui.maintenance.releases.check')"
        :busy="loading"
        :busy-label="t('ui.maintenance.releases.loading')"
        variant="secondary"
        @click="check(true)"
      />
    </InlineAlert>
  </div>
</template>

<style scoped>
.update-notice {
  display: grid;
  gap: var(--vs-space-12);
  margin: var(--vs-space-24);
}
</style>

<script setup lang="ts">
import { computed } from 'vue';
import { storeToRefs } from 'pinia';
import { useI18n } from 'vue-i18n';
import ConfigFieldRenderer from '@/ConfigFieldRenderer.vue';
import NvidiaNvencEncoder from '@/configs/tabs/encoders/NvidiaNvencEncoder.vue';
import IntelQuickSyncEncoder from '@/configs/tabs/encoders/IntelQuickSyncEncoder.vue';
import AmdAmfEncoder from '@/configs/tabs/encoders/AmdAmfEncoder.vue';
import VideotoolboxEncoder from '@/configs/tabs/encoders/VideotoolboxEncoder.vue';
import SoftwareEncoder from '@/configs/tabs/encoders/SoftwareEncoder.vue';
import VAAPIEncoder from '@/configs/tabs/encoders/VAAPIEncoder.vue';
import VulkanEncoder from '@/configs/tabs/encoders/VulkanEncoder.vue';
import { useConfigStore } from '@/stores/config';

const props = defineProps({
  currentTab: { type: String, default: '' },
});

const store = useConfigStore();
const { config, metadata } = storeToRefs(store);
const { t } = useI18n();

// Fallback: if no currentTab provided, show all stacked (modern single page mode)
const showAll = () => !props.currentTab;

const platform = computed(() =>
  (metadata.value?.platform || config.value?.platform || '').toString().toLowerCase(),
);

const gpuList = computed(() => {
  const raw = (metadata.value as any)?.gpus;
  return Array.isArray(raw) ? raw : [];
});

const hasNvidia = computed(() => {
  const metaFlag = (metadata.value as any)?.has_nvidia_gpu;
  if (typeof metaFlag === 'boolean') return metaFlag;
  if (gpuList.value.length) {
    return gpuList.value.some(
      (gpu: any) => Number(gpu?.vendor_id ?? gpu?.vendorId ?? 0) === 0x10de,
    );
  }
  return true;
});

const hasIntel = computed(() => {
  const metaFlag = (metadata.value as any)?.has_intel_gpu;
  if (typeof metaFlag === 'boolean') return metaFlag;
  if (gpuList.value.length) {
    return gpuList.value.some(
      (gpu: any) => Number(gpu?.vendor_id ?? gpu?.vendorId ?? 0) === 0x8086,
    );
  }
  return true;
});

const hasAmd = computed(() => {
  const metaFlag = (metadata.value as any)?.has_amd_gpu;
  if (typeof metaFlag === 'boolean') return metaFlag;
  if (gpuList.value.length) {
    return gpuList.value.some((gpu: any) => {
      const vendor = Number(gpu?.vendor_id ?? gpu?.vendorId ?? 0);
      return vendor === 0x1002 || vendor === 0x1022;
    });
  }
  return true;
});

const shouldShowNvenc = computed(() => (showAll() || props.currentTab === 'nv') && hasNvidia.value);
const shouldShowQsv = computed(
  () => (showAll() || props.currentTab === 'qsv') && hasIntel.value && platform.value === 'windows',
);
const shouldShowAmd = computed(
  () => (showAll() || props.currentTab === 'amd') && hasAmd.value && platform.value === 'windows',
);
const shouldShowVideotoolbox = computed(
  () => (showAll() || props.currentTab === 'vt') && platform.value === 'macos',
);
const shouldShowVaapi = computed(
  () => (showAll() || props.currentTab === 'vaapi') && platform.value === 'linux',
);
const shouldShowVulkan = computed(
  () => (showAll() || props.currentTab === 'vulkan') && platform.value === 'linux',
);
const shouldShowSoftware = computed(() => showAll() || props.currentTab === 'sw');
</script>

<template>
  <div class="config-page space-y-6">
    <div class="space-y-4">
      <ConfigFieldRenderer setting-key="capture" v-model="config.capture" />
      <ConfigFieldRenderer setting-key="encoder" v-model="config.encoder" />

      <section
        v-if="platform === 'windows' && hasNvidia"
        class="space-y-4 rounded-xl border border-dark/35 p-4 dark:border-light/25"
      >
        <div class="space-y-1">
          <h3 class="text-sm font-medium">{{ $t('config.rtx_hdr_title') }}</h3>
          <p class="text-[11px] opacity-70">{{ $t('config.rtx_hdr_intro') }}</p>
        </div>
        <div class="grid gap-4">
          <div class="grid gap-3 md:grid-cols-2">
            <ConfigFieldRenderer
              setting-key="rtx_hdr_peak_brightness"
              v-model="config.rtx_hdr_peak_brightness"
              :desc="t('config.rtx_hdr_peak_brightness_desc')"
            />
            <ConfigFieldRenderer
              setting-key="rtx_hdr_sdr_brightness"
              v-model="config.rtx_hdr_sdr_brightness"
              :desc="t('config.rtx_hdr_sdr_brightness_desc')"
            />
            <ConfigFieldRenderer
              setting-key="rtx_hdr_middle_gray"
              v-model="config.rtx_hdr_middle_gray"
              :desc="t('config.rtx_hdr_middle_gray_desc')"
            />
            <ConfigFieldRenderer
              setting-key="rtx_hdr_contrast"
              v-model="config.rtx_hdr_contrast"
              :desc="t('config.rtx_hdr_contrast_desc')"
            />
            <ConfigFieldRenderer
              setting-key="rtx_hdr_saturation"
              v-model="config.rtx_hdr_saturation"
              :desc="t('config.rtx_hdr_saturation_desc')"
            />
          </div>
        </div>
      </section>
    </div>

    <div v-if="shouldShowNvenc" class="encoder-outline">
      <NvidiaNvencEncoder />
    </div>

    <div v-if="shouldShowQsv" class="encoder-outline">
      <IntelQuickSyncEncoder />
    </div>

    <AmdAmfEncoder v-if="shouldShowAmd" />
    <VideotoolboxEncoder v-if="shouldShowVideotoolbox" />
    <VAAPIEncoder v-if="shouldShowVaapi" />
    <VulkanEncoder v-if="shouldShowVulkan" />

    <div v-if="shouldShowSoftware" class="encoder-outline">
      <SoftwareEncoder />
    </div>
  </div>
</template>

<style scoped>
.encoder-outline {
  @apply border border-dark/35 dark:border-light/25 rounded-xl p-4 bg-light/60 dark:bg-dark/40 space-y-4;
}
</style>

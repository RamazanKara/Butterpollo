<script lang="ts">
  import { onMount } from 'svelte';
  import Badge from '../Badge.svelte';
  import Button from '../Button.svelte';
  import Panel from '../Panel.svelte';
  import LoadState from './LoadState.svelte';
  import { api, type VulkanLayerStatus } from '../../lib/api';
  import { failed, notify } from '../../lib/feedback.svelte';
  import { reason } from './util';

  let layer = $state<VulkanLayerStatus | null>(null);
  let loadError = $state('');
  let busy = $state(false);

  async function load() {
    try {
      layer = await api.health.vulkanLayer();
      loadError = '';
    } catch (error) {
      loadError = reason(error);
    }
  }

  async function register() {
    busy = true;
    try {
      layer = await api.health.registerVulkanLayer();
      notify(layer.installed ? 'Vulkan layer registered.' : 'Registration finished, but the layer is still not registered.', layer.installed ? 'ok' : 'warn');
    } catch (error) {
      failed('Registering the Vulkan layer failed', error);
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

{#snippet yes(value: boolean, on: string, off: string, offTone: 'neutral' | 'warn' = 'neutral')}
  <Badge tone={value ? 'ok' : offTone}>{value ? on : off}</Badge>
{/snippet}

<Panel title="Vulkan HDR support" description="The Vulkan layer lets games using Vulkan graphics show high dynamic range (HDR) on virtual displays.">
  {#if layer}
    <div class="stack">
      <dl class="facts">
        <dt>Registered</dt>
        <dd>{@render yes(layer.installed, 'Yes', 'No', 'warn')}</dd>
        <dt>Setting</dt>
        <dd>{@render yes(layer.enabled, 'On', 'Off')}</dd>
        <dt>Layer files</dt>
        <dd>{@render yes(layer.available, 'Present', 'Missing', 'warn')}</dd>
        <dt>In use</dt>
        <dd>{@render yes(layer.active, 'Yes, by an HDR stream', 'No')}</dd>
      </dl>
      {#if !layer.installed}
        <div class="row">
          <Button {busy} onclick={register}>Register</Button>
          {#if !layer.available}
            <span class="muted hint">The layer files are missing; reinstalling Rubylight restores them.</span>
          {/if}
        </div>
      {/if}
    </div>
  {:else}
    <LoadState error={loadError} what="the Vulkan layer status" />
  {/if}
</Panel>

<style>
  .hint {
    font-size: var(--text-xs);
  }
</style>

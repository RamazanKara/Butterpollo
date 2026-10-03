<script lang="ts">
  import { api } from '../../lib/api';
  import { coverVersion, initials } from './app';

  let {
    name,
    uuid,
    path = '',
    src: override,
    alt = '',
  }: {
    name: string;
    uuid?: string;
    /** The app's 'image-path'; empty means no cover. */
    path?: string;
    /** Show this image instead of the host's copy (an unsaved choice). */
    src?: string;
    alt?: string;
  } = $props();

  const src = $derived(override || (uuid && path ? api.apps.coverUrl(uuid, coverVersion(path)) : ''));
  let broken = $state('');
  const letters = $derived(initials(name));
</script>

<div class="cover">
  {#if src && broken !== src}
    <img {src} {alt} loading="lazy" draggable="false" onerror={() => (broken = src)} />
  {:else}
    <span class="initials" class:long={letters.length > 1} aria-hidden="true">{letters}</span>
  {/if}
</div>

<style>
  .cover {
    aspect-ratio: 2 / 3;
    width: 100%;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--sunken);
    display: grid;
    place-items: center;
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .initials {
    font-family: var(--font-display);
    font-weight: 600;
    font-size: 34px;
    letter-spacing: 0.02em;
    color: var(--muted);
    user-select: none;
  }
  .long {
    font-size: 30px;
  }
</style>

<script lang="ts">
  import { onMount } from 'svelte';
  import Icon, { type IconName } from './components/Icon.svelte';
  import ConfirmHost from './components/ConfirmHost.svelte';
  import Toaster from './components/Toaster.svelte';
  import PairingBanner from './components/PairingBanner.svelte';
  import { api, setSignedOutHandler } from './lib/api';
  import { failed } from './lib/feedback.svelte';
  import { poll } from './lib/format';
  import { link, match, navigate, route } from './lib/router.svelte';
  import { checkSession, refreshPending, session, signedOut } from './lib/session.svelte';
  import { applyTheme, storedTheme, type Theme } from './lib/theme';
  import Overview from './pages/Overview.svelte';
  import Library from './pages/Library.svelte';
  import Devices from './pages/Devices.svelte';
  import Settings from './pages/Settings.svelte';
  import Logs from './pages/Logs.svelte';
  import Maintenance from './pages/Maintenance.svelte';
  import Tokens from './pages/Tokens.svelte';
  import Login from './pages/Login.svelte';
  import Setup from './pages/Setup.svelte';
  import NotFound from './pages/NotFound.svelte';

  const nav: { path: string; label: string; icon: IconName }[] = [
    { path: '/', label: 'Overview', icon: 'overview' },
    { path: '/library', label: 'Library', icon: 'library' },
    { path: '/devices', label: 'Devices', icon: 'devices' },
    { path: '/settings', label: 'Settings', icon: 'settings' },
    { path: '/logs', label: 'Logs', icon: 'logs' },
    { path: '/maintenance', label: 'Maintenance', icon: 'maintenance' },
    { path: '/tokens', label: 'API tokens', icon: 'key' },
  ];

  let theme = $state<Theme>(storedTheme());
  let menuOpen = $state(false);

  const current = (path: string) =>
    path === '/' ? route.path === '/' : route.path === path || route.path.startsWith(`${path}/`);

  const Page = $derived.by(() => {
    if (route.path === '/login') return Login;
    if (route.path === '/setup') return Setup;
    if (route.path === '/') return Overview;
    if (match('/library') || match('/library/:uuid')) return Library;
    if (route.path === '/devices') return Devices;
    if (match('/settings') || match('/settings/:section')) return Settings;
    if (route.path === '/logs') return Logs;
    if (route.path === '/maintenance') return Maintenance;
    if (route.path === '/tokens') return Tokens;
    return NotFound;
  });
  const bare = $derived(route.path === '/login' || route.path === '/setup' || session.state !== 'signed-in');

  function cycleTheme() {
    theme = theme === 'system' ? 'light' : theme === 'light' ? 'dark' : 'system';
    applyTheme(theme);
  }
  async function signOut() {
    try {
      await api.auth.logout();
    } catch (error) {
      failed('Signing out failed', error);
    }
    signedOut();
  }

  onMount(() => {
    applyTheme(theme);
    setSignedOutHandler(signedOut);
    void checkSession();
    return poll(refreshPending, 3000);
  });
  $effect(() => {
    // Close the mobile menu after navigating.
    void route.path;
    menuOpen = false;
  });
</script>

{#if session.state === 'checking'}
  <div class="loading" aria-busy="true"></div>
{:else if bare}
  <Page />
{:else}
  <div class="shell">
    <aside class:open={menuOpen}>
      <div class="brand-row">
        <a class="brand" href="/" use:link>
          <span class="mark" aria-hidden="true">R</span>
          <span>Rubylight</span>
        </a>
        <button class="menu" aria-label="Menu" aria-expanded={menuOpen} onclick={() => (menuOpen = !menuOpen)}>
          <Icon name="menu" />
        </button>
      </div>
      <nav aria-label="Main">
        {#each nav as item (item.path)}
          <a href={item.path} use:link aria-current={current(item.path) ? 'page' : undefined}>
            <Icon name={item.icon} />
            {item.label}
          </a>
        {/each}
      </nav>
      <div class="foot">
        {#if session.metadata}
          <div class="host">
            <span class="name">{session.metadata.host_name}</span>
            <span class="muted num">{session.metadata.pc_address} · {session.metadata.version}</span>
          </div>
        {/if}
        <div class="row">
          <button class="foot-button" onclick={cycleTheme} title="Theme: {theme}">
            <Icon name={theme === 'dark' ? 'moon' : theme === 'light' ? 'sun' : 'monitor'} size={16} />
            <span class="cap">{theme}</span>
          </button>
          <button class="foot-button" onclick={signOut}>
            <Icon name="signout" size={16} />
            Sign out
          </button>
        </div>
      </div>
    </aside>
    <main>
      {#if session.pending.length && route.path !== '/devices'}
        <PairingBanner requests={session.pending} onpaired={() => navigate('/devices')} />
      {/if}
      <Page />
    </main>
  </div>
{/if}

<ConfirmHost />
<Toaster />

<style>
  .loading {
    min-height: 100vh;
  }
  .shell {
    display: grid;
    grid-template-columns: var(--nav-width) minmax(0, 1fr);
    min-height: 100vh;
  }
  aside {
    position: sticky;
    top: 0;
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
    padding: var(--space-5) var(--space-3) var(--space-4);
    border-right: 1px solid var(--line);
    background: var(--panel);
  }
  .brand-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0 var(--space-2);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--font-display);
    font-weight: 650;
    font-size: 17px;
    text-decoration: none;
    letter-spacing: -0.01em;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 6px;
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 800;
    font-size: 15px;
  }
  .menu {
    display: none;
    border: 0;
    background: none;
    padding: 4px;
    cursor: pointer;
  }
  nav {
    display: grid;
    gap: 2px;
  }
  nav a {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: var(--radius);
    color: var(--ink-2);
    text-decoration: none;
    font-weight: 520;
  }
  nav a:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  nav a[aria-current='page'] {
    background: var(--accent-soft);
    color: var(--ink);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .foot {
    margin-top: auto;
    display: grid;
    gap: var(--space-3);
    padding: 0 var(--space-2);
  }
  .host {
    display: grid;
    gap: 2px;
    font-size: var(--text-sm);
  }
  .host .name {
    font-weight: 600;
  }
  .host .muted {
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .foot-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: none;
    padding: 6px 9px;
    font-size: var(--text-xs);
    color: var(--ink-2);
    cursor: pointer;
  }
  .foot-button:hover {
    background: var(--sunken);
  }
  .cap {
    text-transform: capitalize;
  }
  main {
    padding: var(--space-6) var(--space-6) var(--space-7);
    max-width: 1320px;
    width: 100%;
    display: grid;
    align-content: start;
    gap: var(--space-5);
  }
  @media (max-width: 860px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto 1fr;
    }
    aside {
      position: sticky;
      height: auto;
      z-index: 10;
      padding: var(--space-3) var(--space-3);
      gap: var(--space-3);
      border-right: 0;
      border-bottom: 1px solid var(--line);
    }
    .menu {
      display: block;
    }
    nav,
    .foot {
      display: none;
    }
    aside.open nav,
    aside.open .foot {
      display: grid;
    }
    main {
      padding: var(--space-5) var(--space-4) var(--space-6);
    }
  }
</style>

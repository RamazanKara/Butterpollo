<script lang="ts">
  import { onMount } from 'svelte';
  import PageHeader from '../components/PageHeader.svelte';
  import CreateToken from '../components/tokens/CreateToken.svelte';
  import TokenList from '../components/tokens/TokenList.svelte';
  import { api, type ApiToken } from '../lib/api';

  let tokens = $state<ApiToken[] | null>(null);
  let error = $state('');

  async function load() {
    try {
      tokens = (await api.tokens.list()).tokens;
      error = '';
    } catch (failure) {
      error = failure instanceof Error ? failure.message : String(failure);
    }
  }

  onMount(load);
</script>

<div class="page">
  <PageHeader title="API tokens" subtitle="Give scripts access to selected API paths without sharing your password." />
  <div class="stack">
    <p class="notice usage">
      <span>Scripts send the token with each request: <code>Authorization: Bearer &lt;token&gt;</code></span>
    </p>
    <TokenList {tokens} {error} onchange={load} />
    <CreateToken oncreated={load} />
  </div>
</div>

<style>
  .page {
    display: grid;
    gap: var(--space-5);
    min-width: 0;
    max-width: 1040px;
  }
  .stack {
    gap: var(--space-5);
  }
  .usage {
    overflow-wrap: anywhere;
  }
  code {
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--panel);
    white-space: normal;
  }
</style>

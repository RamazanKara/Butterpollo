<script lang="ts">
  import Button from '../components/Button.svelte';
  import EmptyState from '../components/EmptyState.svelte';
</script>

<EmptyState icon="search" title="Page not found">
  <p>This address is not part of the console.</p>
  <Button href="/">Go to the overview</Button>
</EmptyState>

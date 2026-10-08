<script lang="ts">
  let {
    checked = $bindable(false),
    label,
    hint,
    disabled = false,
    onchange,
  }: {
    checked?: boolean;
    label: string;
    hint?: string;
    disabled?: boolean;
    onchange?: (checked: boolean) => void;
  } = $props();
</script>

<label class="toggle" class:disabled>
  <input
    type="checkbox"
    role="switch"
    bind:checked
    {disabled}
    onchange={() => onchange?.(checked)}
  />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
  <span class="text">
    <span class="label">{label}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </span>
</label>

<style>
  .toggle {
    display: flex;
    gap: var(--space-3);
    align-items: flex-start;
    cursor: pointer;
  }
  .disabled {
    opacity: 0.55;
    cursor: default;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }
  .track {
    flex: none;
    width: 34px;
    height: 20px;
    border-radius: 10px;
    background: var(--line-strong);
    position: relative;
    margin-top: 1px;
    transition: background 0.12s;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--panel);
    box-shadow: 0 1px 2px rgb(0 0 0 / 25%);
    transition: transform 0.12s;
  }
  input:checked + .track {
    background: var(--accent);
  }
  input:checked + .track .thumb {
    transform: translateX(14px);
  }
  input:focus-visible + .track {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .text {
    min-width: 0;
    display: grid;
    gap: 2px;
  }
  .label {
    font-weight: 560;
    font-size: var(--text-sm);
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.45;
  }
</style>

<script setup lang="ts">
defineProps<{ modelValue: boolean; label: string; description?: string; disabled?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>

<template>
  <label class="app-switch-field" :class="{ 'is-disabled': disabled }">
    <input class="app-switch-input" type="checkbox" role="switch" :checked="modelValue" :aria-checked="modelValue" :aria-label="label" :disabled="disabled" @change="emit('update:modelValue', ($event.target as HTMLInputElement).checked)">
    <span class="app-switch-track" aria-hidden="true"><span /></span>
    <span class="app-switch-copy"><span>{{ label }}</span><small v-if="description">{{ description }}</small></span>
  </label>
</template>

<style scoped>
.app-switch-field { position: relative; display: inline-flex; align-items: center; gap: 10px; margin: 0; padding: 0; min-height: 32px; color: var(--text); font-size: 14px; font-weight: 400; cursor: pointer; }
.app-switch-field .app-switch-input { position: absolute; left: 0; top: 50%; transform: translateY(-50%); width: 40px; height: 22px; margin: 0; opacity: 0; cursor: inherit; }
.app-switch-track { display: inline-flex; align-items: center; flex: 0 0 40px; width: 40px; height: 22px; padding: 2px; border-radius: 11px; background: var(--line-strong); transition: background .15s ease; }
.app-switch-track > span { width: 18px; height: 18px; border-radius: 50%; background: var(--panel); box-shadow: 0 1px 2px rgb(0 0 0 / 12%); transition: transform .15s ease; }
.app-switch-input:checked + .app-switch-track { background: var(--accent); }
.app-switch-input:checked + .app-switch-track > span { transform: translateX(18px); }
.app-switch-input:focus-visible + .app-switch-track { outline: 2px solid var(--accent); outline-offset: 3px; }
.app-switch-copy { display: grid; gap: 4px; min-width: 0; line-height: 1.5; }
.app-switch-copy small { font-size: 12px; color: var(--muted); }
.app-switch-field.is-disabled { opacity: .5; cursor: not-allowed; }
@media (prefers-reduced-motion: reduce) { .app-switch-track, .app-switch-track > span { transition: none; } }
</style>

<script setup lang="ts">
import { onBeforeUnmount, watch } from 'vue'
import AppIcon from './AppIcon.vue'
import { useLocale } from '../i18n'

const props = defineProps<{ message: string; kind: 'success' | 'error' }>()
const emit = defineEmits<{ close: [] }>()
const locale = useLocale()
let timer: ReturnType<typeof setTimeout> | undefined
watch(() => [props.message, props.kind], () => {
  clearTimeout(timer)
  timer = setTimeout(() => emit('close'), 6000)
}, { immediate: true })
onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <Teleport to="body">
    <div class="app-toast" :class="kind" :role="kind === 'error' ? 'alert' : 'status'" aria-atomic="true">
      <AppIcon class="app-toast-status" :name="kind === 'error' ? 'status-error' : 'status-success'" :size="18" />
      <span>{{ message }}</span>
      <button type="button" :aria-label="locale.text('关闭提示', 'Dismiss notification')" @click="emit('close')"><AppIcon name="close" :size="16" weight="regular" /></button>
    </div>
  </Teleport>
</template>

<style scoped>
.app-toast { position: fixed; z-index: 200; top: max(20px, env(safe-area-inset-top)); left: 50%; transform: translateX(-50%); display: flex; align-items: flex-start; gap: 10px; width: max-content; max-width: min(560px, calc(100vw - 40px)); padding: 12px 16px; border: 1px solid; border-radius: 4px; box-shadow: var(--shadow); font-size: 14px; line-height: 1.6; }
.app-toast.error { color: #e05267; background: #fff1f2; border-color: #fbd5dc; }
.app-toast.success { color: #2da66e; background: #effaf3; border-color: #ccebd8; }
:global(:root[data-theme="dark"] .app-toast.error) { color: #f18a99; background: #34242a; border-color: #633640; }
:global(:root[data-theme="dark"] .app-toast.success) { color: #70d5a1; background: #20352b; border-color: #355b46; }
.app-toast .app-toast-status { flex: 0 0 18px; margin-top: 3px; }
.app-toast span { min-width: 0; overflow-wrap: anywhere; }
.app-toast button { flex: 0 0 auto; display: grid; place-items: center; width: 24px; height: 24px; padding: 0; border: 0; border-radius: 3px; color: var(--muted-2); background: transparent; cursor: pointer; }
.app-toast :deep(.app-icon) { color: currentColor !important; }
.app-toast button:hover { background: rgb(127 127 127 / 12%); }
</style>

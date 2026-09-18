<script setup lang="ts">
import AppSwitch from './AppSwitch.vue'
import type { TrafficQuota } from '../api/admin'
import { useLocale } from '../i18n'
const props = defineProps<{ modelValue: TrafficQuota; label: string; disabled?: boolean; flat?: boolean; downloadOnly?: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: TrafficQuota] }>()
const locale = useLocale()
function update(direction: 'upload' | 'download', event: Event): void {
  const value = Number((event.target as HTMLInputElement).value)
  emit('update:modelValue', { ...props.modelValue, [direction]: Math.round(value * 1024 ** 3) })
}
</script>
<template>
  <fieldset class="traffic-quota" :aria-label="label" :disabled="disabled">
    <legend v-if="!flat">{{ label }}</legend>
    <AppSwitch :model-value="modelValue.enabled" :label="locale.text('启用流量限制', 'Enable traffic limits')" :disabled="disabled" @update:model-value="emit('update:modelValue', { ...modelValue, enabled: $event })" />
    <div class="quota-inputs" :class="{ 'download-only': downloadOnly }">
      <label v-for="direction in (downloadOnly ? ['download'] as const : ['upload', 'download'] as const)" :key="direction">
        <span>{{ direction === 'upload' ? locale.text('上传额度', 'Upload allowance') : locale.text('下载额度', 'Download allowance') }}</span>
        <span class="input-with-unit"><input class="input" :value="modelValue[direction] / 1024 ** 3" :disabled="!modelValue.enabled || disabled" type="number" min="0" max="8388607" step="any" @input="update(direction, $event)"><span>GiB</span></span>
      </label>
    </div>
    <small>{{ locale.text('0 表示该方向不限额；关闭限制仍记录用量。', '0 means unlimited in that direction. Usage is still recorded when limits are off.') }}</small>
  </fieldset>
</template>
<style scoped>
.traffic-quota { min-width: 0; margin: 0; display: grid; gap: 16px; padding: 0; border: 0; }
.traffic-quota legend { padding: 0 0 16px; font-size: 14px; color: var(--text); }
.traffic-quota .input { box-sizing: border-box; width: 100%; min-width: 0; height: 36px; min-height: 36px; font-size: 14px; font-weight: 400; border-radius: 3px; }
.quota-inputs { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
.quota-inputs.download-only { grid-template-columns: 1fr; }
.quota-inputs label { min-width: 0; display: grid; gap: 8px; font-size: 14px; font-weight: 400; color: var(--muted); }
.traffic-quota small { color: var(--muted); font-size: 12px; }
@media(max-width: 520px) { .quota-inputs { grid-template-columns: 1fr; } }
</style>

<script setup lang="ts">
import { computed } from 'vue'
import { useLocale } from '../i18n'

const props = defineProps<{
  usage: { download: number; upload: number }
  quota: { enabled: boolean; download: number; upload: number }
}>()
const locale = useLocale()
const directions = computed(() => [
  { key: 'download' as const, label: locale.text('下载', 'Download'), radius: 64, color: '#10b981', arrow: '↓' },
  { key: 'upload' as const, label: locale.text('上传', 'Upload'), radius: 53, color: '#6860f5', arrow: '↑' },
])
function bytes(value: number): string {
  if (!value) return '0 B'
  const power = Math.min(4, Math.floor(Math.log(value) / Math.log(1024)))
  return `${(value / 1024 ** power).toLocaleString(undefined, { maximumFractionDigits: 2 })} ${['B', 'KiB', 'MiB', 'GiB', 'TiB'][power]}`
}
function allowance(key: 'download' | 'upload'): string {
  return props.quota.enabled && props.quota[key] > 0 ? bytes(props.quota[key]) : '∞'
}
function percent(key: 'download' | 'upload'): number {
  return props.quota.enabled && props.quota[key] > 0
    ? Math.min(100, Math.max(0, props.usage[key] / props.quota[key] * 100)) : 0
}
function percentageLabel(key: 'download' | 'upload'): string {
  if (!props.quota.enabled || props.quota[key] === 0) return '∞'
  return `${percent(key).toLocaleString(undefined, { maximumFractionDigits: 1 })}%`
}
</script>

<template>
  <div class="user-traffic-rings">
    <div class="traffic-rings-chart">
      <svg data-chart="user-traffic" viewBox="0 0 144 144" aria-hidden="true">
        <g v-for="direction in directions" :key="direction.key">
          <circle class="ring-track" cx="72" cy="72" :r="direction.radius" />
          <circle class="ring-progress" cx="72" cy="72" :r="direction.radius" pathLength="100" :stroke="direction.color" :stroke-dasharray="`${percent(direction.key)} 100`" :opacity="percent(direction.key) ? 1 : 0" />
        </g>
      </svg>
      <div class="traffic-rings-center">
        <template v-for="(direction, index) in directions" :key="direction.key">
          <span v-if="index" class="traffic-rings-divider" aria-hidden="true" />
          <strong :style="{ color: direction.color }">{{ direction.arrow }}{{ percentageLabel(direction.key) }}</strong>
        </template>
      </div>
    </div>
    <div v-for="direction in directions" :key="direction.key" class="traffic-rings-usage" role="meter" :aria-label="direction.label" aria-valuemin="0" aria-valuemax="100" :aria-valuenow="percent(direction.key)" :aria-valuetext="`${bytes(usage[direction.key])} / ${allowance(direction.key)}`">
      <span :style="{ color: direction.color }">{{ direction.arrow }} {{ direction.label }}</span>
      <span>{{ bytes(usage[direction.key]) }} / {{ allowance(direction.key) }}</span>
    </div>
  </div>
</template>

<style scoped>
.user-traffic-rings { width: 100%; font-size: 12px; }
.traffic-rings-chart { position: relative; width: 144px; height: 144px; margin: 0 auto 10px; }
.traffic-rings-chart svg { width: 100%; height: 100%; transform: rotate(-90deg); }
.traffic-rings-chart circle { fill: none; stroke-width: 6; }
.ring-track { stroke: var(--line); }
.ring-progress { stroke-linecap: round; }
.traffic-rings-center { position: absolute; inset: 0; display: flex; flex-direction: column; justify-content: center; align-items: center; gap: 6px; font-variant-numeric: tabular-nums; }
.traffic-rings-divider { width: 64px; border-top: 1px solid var(--muted); }
.traffic-rings-center strong { font-size: 13px; }
.traffic-rings-usage { display: flex; justify-content: space-between; gap: 12px; margin-top: 8px; font-variant-numeric: tabular-nums; }
</style>

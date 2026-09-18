<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { getTraffic, saveTraffic } from '../../shared/api/admin'
import type { TrafficInfo, TrafficQuota, TrafficUsage } from '../../shared/api/admin'
import AppDatePicker from '../../shared/components/AppDatePicker.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import AppSelect from '../../shared/components/AppSelect.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import TrafficQuotaFields from '../../shared/components/TrafficQuotaFields.vue'
import { useLocale } from '../../shared/i18n'

const props = withDefaults(defineProps<{ mode?: 'dashboard' | 'settings' }>(), { mode: 'dashboard' })
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()
const info = ref<TrafficInfo>()
const loading = ref(false)
const saving = ref(false)
const editing = ref(false)
const message = ref('')
const kind = ref<'error' | 'success'>('error')
const revision = ref(0)
const start = ref('')
const end = ref('')
const total = ref<TrafficQuota>({ enabled: false, upload: 0, download: 0 })
const guest = ref<TrafficQuota>({ enabled: false, upload: 0, download: 0 })
const usersQuota = ref<TrafficQuota>({ enabled: false, upload: 0, download: 0 })
const unit = ref<'hours' | 'days' | 'months'>('months')
const every = ref(1)
const anchor = ref('')
const directions = ['download', 'upload'] as const
type Direction = typeof directions[number]
type TrafficMeter = { id: string; label: string; direction: Direction; use: TrafficUsage; quota: TrafficQuota }

function bytes(value: number): string {
  if (!value) return '0 B'
  const power = Math.max(0, Math.min(4, Math.floor(Math.log(value) / Math.log(1024))))
  return (value / 1024 ** power).toLocaleString(undefined, { maximumFractionDigits: 2 }) + ' ' + ['B', 'KiB', 'MiB', 'GiB', 'TiB'][power]
}
function directionLabel(direction: Direction): string {
  return direction === 'upload' ? locale.text('上传', 'Upload') : locale.text('下载', 'Download')
}
const users = computed(() => info.value?.users_total ?? { upload: 0, download: 0 })
const meters = computed<TrafficMeter[]>(() => info.value ? [
  { id: 'total-download', label: locale.text('总流量下载', 'Total download'), direction: 'download', use: info.value.total, quota: info.value.settings.total },
  { id: 'total-upload', label: locale.text('总流量上传', 'Total upload'), direction: 'upload', use: info.value.total, quota: info.value.settings.total },
  { id: 'guest-download', label: locale.text('访客下载', 'Guest download'), direction: 'download', use: info.value.guest, quota: info.value.settings.guest },
  { id: 'users-download', label: locale.text('用户下载', 'User download'), direction: 'download', use: users.value, quota: info.value.settings.users_total },
  { id: 'users-upload', label: locale.text('用户上传', 'User upload'), direction: 'upload', use: users.value, quota: info.value.settings.users_total },
] : [])
function quotaAmount(quota: TrafficQuota, direction: Direction): string {
  return quota.enabled && quota[direction] ? bytes(quota[direction]) : '∞'
}
function percent(use: TrafficUsage, quota: TrafficQuota, direction: Direction): number {
  const upper = quota.enabled ? quota[direction] : 0
  return upper ? Math.min(100, use[direction] / upper * 100) : 0
}
function dialValue(use: TrafficUsage, quota: TrafficQuota, direction: Direction): string {
  return percent(use, quota, direction).toLocaleString(undefined, { maximumFractionDigits: 1 }) + '%'
}
const days = computed(() => Object.entries(info.value?.days ?? {}).map(([date, use]) => ({ date, ...use })))
const sum = computed(() => days.value.reduce((result, day) => ({
  upload: result.upload + day.upload,
  download: result.download + day.download,
}), { upload: 0, download: 0 }))
const historyTotal = computed(() => sum.value.upload + sum.value.download)
const maximum = computed(() => Math.max(1, ...days.value.flatMap(day => [day.upload, day.download])))
const selected = ref<string>()
const selectedDay = computed(() => days.value.find(day => day.date === selected.value))
const dayTooltipLeft = ref(0)
const dayTooltipTop = ref(0)
const dayTooltipWidth = ref(174)
const selectedShare = ref<Direction>()
const shareTooltipLeft = ref(80)
const rangeMode = ref<'today' | '7' | '30' | 'custom'>('30')
const downloadShare = computed(() => historyTotal.value ? sum.value.download / historyTotal.value * 100 : 0)
const uploadShare = computed(() => 100 - downloadShare.value)
const today = computed(() => {
  const date = new Date()
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
})
function dateBefore(days: number): string {
  const date = new Date()
  date.setDate(date.getDate() - days)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}
async function selectRange(mode: typeof rangeMode.value): Promise<void> {
  rangeMode.value = mode
  if (mode === 'custom') return
  end.value = today.value
  start.value = mode === 'today' ? today.value : dateBefore(Number(mode) - 1)
  await refresh()
}
function selectDay(date: string, event: Event): void {
  selected.value = date
  const bar = event.currentTarget as HTMLElement
  const chart = bar.closest<HTMLElement>('.bar-chart')
  if (!chart) return
  const barRect = bar.getBoundingClientRect()
  const chartRect = chart.getBoundingClientRect()
  const gap = 10
  const tooltipWidth = 174
  const rightSpace = chartRect.right - barRect.right - gap
  const leftSpace = barRect.left - chartRect.left - gap
  const placeRight = rightSpace >= tooltipWidth || (leftSpace < tooltipWidth && rightSpace >= leftSpace)
  const available = placeRight ? rightSpace : leftSpace
  dayTooltipWidth.value = Math.max(96, Math.min(tooltipWidth, available))
  dayTooltipLeft.value = placeRight
    ? barRect.right - chartRect.left + gap
    : barRect.left - chartRect.left - gap - dayTooltipWidth.value
  const pointerY = event instanceof MouseEvent ? event.clientY : barRect.top + barRect.height / 2
  dayTooltipTop.value = Math.max(0, Math.min(chartRect.height - 136, pointerY - chartRect.top - 68))
}
function selectShare(event: MouseEvent): void {
  if (!historyTotal.value) return
  const element = event.currentTarget as HTMLElement
  const rect = element.getBoundingClientRect()
  const x = event.clientX - (rect.left + rect.width / 2)
  const y = event.clientY - (rect.top + rect.height / 2)
  const radius = Math.min(rect.width, rect.height) / 2
  const distance = Math.hypot(x, y)
  if (radius && (distance < radius * .65 || distance > radius * .98)) {
    selectedShare.value = undefined
    return
  }
  const degrees = (Math.atan2(x, -y) * 180 / Math.PI + 360) % 360
  const direction = degrees < sum.value.download / historyTotal.value * 360 ? 'download' : 'upload'
  if (selectedShare.value === direction) return
  selectedShare.value = direction
  const panel = element.closest<HTMLElement>('.share-panel')
  if (!panel) return
  const panelRect = panel.getBoundingClientRect()
  const midpoint = direction === 'download' ? downloadShare.value * 1.8 : downloadShare.value * 3.6 + uploadShare.value * 1.8
  const sectorX = rect.left + rect.width / 2 + Math.sin(midpoint * Math.PI / 180) * rect.width * .42
  const halfTooltip = Math.min(90, panelRect.width / 2)
  shareTooltipLeft.value = Math.max(halfTooltip, Math.min(panelRect.width - halfTooltip, sectorX - panelRect.left))
}
function sharePercent(direction: Direction): string {
  return historyTotal.value ? (sum.value[direction] / historyTotal.value * 100).toLocaleString(undefined, { maximumFractionDigits: 1 }) + '%' : '0%'
}
function feedback(error: unknown): void {
  kind.value = 'error'
  message.value = error instanceof Error ? error.message : locale.text('请求失败', 'Request failed')
  revision.value++
}
let requestId = 0
async function refresh(): Promise<void> {
  const id = ++requestId
  loading.value = true
  try {
    const result = await getTraffic(start.value || undefined, end.value || undefined)
    if (id !== requestId) return
    info.value = result
    selected.value = undefined
    selectedShare.value = undefined
  } catch (error) {
    if (id === requestId) feedback(error)
  } finally {
    if (id === requestId) loading.value = false
  }
}
async function openEditor(): Promise<void> {
  if (!info.value) await refresh()
  if (!info.value) return
  total.value = { ...info.value.settings.total }
  guest.value = { ...info.value.settings.guest }
  usersQuota.value = { ...info.value.settings.users_total }
  const cycle = info.value.settings.cycle
  unit.value = cycle.unit
  every.value = cycle.every
  const date = new Date(cycle.anchor * 1000)
  anchor.value = new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 10)
  editing.value = true
}
async function save(): Promise<void> {
  if (saving.value) return
  const timestamp = new Date(`${anchor.value}T00:00:00`)
  if (!Number.isFinite(timestamp.getTime()) || !Number.isInteger(every.value) || every.value < 1 || every.value > 120
    || [total.value, guest.value, usersQuota.value].some(quota => directions.some(direction => !Number.isSafeInteger(quota[direction]) || quota[direction] < 0))) {
    feedback(new Error(locale.text('请填写有效的额度、周期和起始日期', 'Enter a valid allowance, interval and start date')))
    return
  }
  saving.value = true
  try {
    await saveTraffic({
      total: total.value,
      guest: guest.value,
      users_total: usersQuota.value,
      cycle: {
        unit: unit.value,
        every: every.value,
        anchor: Math.floor(timestamp.getTime() / 1000),
        offset_minutes: -timestamp.getTimezoneOffset(),
      },
    })
    editing.value = false
    kind.value = 'success'
    message.value = locale.text('流量设置已保存', 'Traffic settings saved')
    emit('saved', message.value)
    revision.value++
    await refresh()
  } catch (error) {
    feedback(error)
  } finally {
    saving.value = false
  }
}
onMounted(() => { void (props.mode === 'dashboard' ? selectRange('30') : refresh()) })
defineExpose({ openEditor })
</script>

<template>
  <div class="traffic-panel" :class="`traffic-mode-${mode}`">
    <template v-if="mode === 'dashboard'">
      <section class="dashboard-block glass traffic-usage-block" :aria-label="locale.text('流量信息', 'Traffic information')">
        <header class="traffic-usage-heading"><h2>{{ locale.text('流量信息', 'Traffic information') }}</h2></header>
        <p v-if="!info">{{ loading ? locale.text('正在读取流量统计…', 'Loading traffic…') : locale.text('暂未取得流量统计', 'Traffic statistics unavailable') }}</p>
        <div v-if="info" class="traffic-meters">
          <article v-for="meter in meters" :key="meter.id" class="traffic-meter" :class="`meter-${meter.direction}`">
            <h3>{{ meter.label }}</h3>
            <div class="usage-dial" role="meter" aria-valuemin="0" aria-valuemax="100" :aria-valuenow="percent(meter.use, meter.quota, meter.direction)" :aria-label="meter.label" :aria-valuetext="bytes(meter.use[meter.direction])">
              <svg data-chart="traffic-usage" viewBox="0 0 120 120" aria-hidden="true">
                <circle class="ring-track" cx="60" cy="60" r="52" />
                <circle class="ring-value" cx="60" cy="60" r="52" pathLength="100" :stroke-dasharray="percent(meter.use, meter.quota, meter.direction) + ' 100'" :opacity="percent(meter.use, meter.quota, meter.direction) ? 1 : 0" />
              </svg>
              <div class="dial-center"><strong>{{ dialValue(meter.use, meter.quota, meter.direction) }}</strong></div>
            </div>
            <p class="meter-amount">{{ bytes(meter.use[meter.direction]) }} / {{ quotaAmount(meter.quota, meter.direction) }}</p>
          </article>
        </div>
      </section>
      <section v-if="info" class="dashboard-block glass traffic-history" :aria-label="locale.text('全站上传下载流量', 'Site upload and download traffic')">
        <header class="history-heading">
          <div><h2>{{ locale.text('流量统计', 'Traffic statistics') }}</h2><p class="traffic-note">{{ locale.text('下次统一重置：', 'Next shared reset: ') }}{{ new Date(info.next_reset * 1000).toLocaleString() }}</p></div>
          <div class="range-controls">
            <div class="range-presets" :aria-label="locale.text('日期范围', 'Date range')">
              <button type="button" :class="{ active: rangeMode === 'today' }" @click="selectRange('today')">{{ locale.text('今日', 'Today') }}</button>
              <button type="button" :class="{ active: rangeMode === '7' }" @click="selectRange('7')">{{ locale.text('近 7 天', 'Last 7 days') }}</button>
              <button type="button" :class="{ active: rangeMode === '30' }" @click="selectRange('30')">{{ locale.text('近 30 天', 'Last 30 days') }}</button>
              <button type="button" :class="{ active: rangeMode === 'custom' }" @click="selectRange('custom')">{{ locale.text('自定义', 'Custom') }}</button>
            </div>
            <form v-if="rangeMode === 'custom'" class="traffic-range" @submit.prevent="refresh">
              <AppDatePicker v-model="start" :label="locale.text('开始日期', 'Start date')" :max="end || today" :show-footer="false" />
              <span aria-hidden="true">至</span>
              <AppDatePicker v-model="end" :label="locale.text('结束日期', 'End date')" :min="start" :max="today" :show-footer="false" />
              <button class="btn secondary" type="submit" :disabled="loading || !start || !end">{{ locale.text('查看', 'View') }}</button>
            </form>
          </div>
        </header>
        <div class="history-layout">
          <section class="trend-panel" :aria-label="locale.text('上传下载趋势', 'Upload and download trend')">
            <h3>{{ locale.text('流量趋势 · 按天', 'Traffic trend · daily') }}</h3>
            <div class="bar-chart">
              <div v-if="selectedDay" class="day-tooltip" role="tooltip" :style="{ left: `${dayTooltipLeft}px`, top: `${dayTooltipTop}px`, width: `${dayTooltipWidth}px` }"><strong>{{ selectedDay.date }}</strong><span class="download-key"><i />{{ directionLabel('download') }}<b>{{ bytes(selectedDay.download) }}</b></span><span class="upload-key"><i />{{ directionLabel('upload') }}<b>{{ bytes(selectedDay.upload) }}</b></span><em>{{ locale.text('合计', 'Total') }}<b>{{ bytes(selectedDay.download + selectedDay.upload) }}</b></em></div>
              <div class="chart-axis"><span>{{ bytes(maximum) }}</span><span>{{ bytes(maximum / 2) }}</span><span>0 B</span></div>
              <div class="chart-days">
                <div v-for="(day, index) in days" :key="day.date" class="chart-day">
                  <button type="button" class="day-bars" :class="{ selected: selected === day.date }" :aria-label="day.date + ' ' + directionLabel('upload') + ' ' + bytes(day.upload) + ', ' + directionLabel('download') + ' ' + bytes(day.download)" @mouseenter="selectDay(day.date, $event)" @mousemove="selectDay(day.date, $event)" @mouseleave="selected = undefined" @focus="selectDay(day.date, $event)" @blur="selected = undefined" @click="selectDay(day.date, $event)">
                    <span class="download" :style="{ height: (day.download / maximum * 100) + '%' }" />
                    <span class="upload" :style="{ height: (day.upload / maximum * 100) + '%' }" />
                  </button>
                  <small>{{ index % Math.max(1, Math.ceil(days.length / 7)) === 0 ? day.date.slice(5) : '' }}</small>
                </div>
              </div>
            </div>
          </section>
          <section class="share-panel" :aria-label="locale.text('上传下载占比', 'Upload and download share')">
            <h3>{{ locale.text('上传／下载占比', 'Upload / download share') }}</h3>
            <div class="share-donut" @mousemove="selectShare" @mouseleave="selectedShare = undefined">
              <svg data-chart="traffic-share" viewBox="0 0 120 120" aria-hidden="true">
                <circle class="share-ring-track" cx="60" cy="60" r="48" pathLength="100" />
                <circle class="share-ring share-ring-download" :class="{ active: selectedShare === 'download' }" cx="60" cy="60" r="48" pathLength="100" :stroke-dasharray="downloadShare + ' ' + (100 - downloadShare)" :opacity="downloadShare ? 1 : 0" />
                <circle class="share-ring share-ring-upload" :class="{ active: selectedShare === 'upload' }" cx="60" cy="60" r="48" pathLength="100" :stroke-dasharray="uploadShare + ' ' + (100 - uploadShare)" :stroke-dashoffset="-downloadShare" :opacity="uploadShare && historyTotal ? 1 : 0" />
              </svg>
              <div><strong>{{ bytes(historyTotal) }}</strong><span>{{ locale.text('总流量', 'Total traffic') }}</span></div>
            </div>
            <div class="share-tooltip-slot"><p v-if="selectedShare" class="share-tooltip" role="tooltip" :style="{ left: `${shareTooltipLeft}px` }">{{ directionLabel(selectedShare) }}：{{ bytes(sum[selectedShare]) }}（{{ sharePercent(selectedShare) }}）</p></div>
            <div class="history-legend" :aria-label="locale.text('流量图例', 'Traffic legend')"><span class="download-key"><i />{{ locale.text('下载', 'Download') }}</span><span class="upload-key"><i />{{ locale.text('上传', 'Upload') }}</span></div>
          </section>
        </div>
      </section>
    </template>

    <AppFeedback :message="message" :kind="kind" :revision="revision" />
    <SettingsDrawer v-if="editing" :title="locale.text('流量设置', 'Traffic settings')" :busy="saving" @close="editing = false">
      <form class="drawer-form traffic-form" @submit.prevent="save">
        <TrafficQuotaFields v-model="total" :label="locale.text('全站额度', 'Site allowance')" :disabled="saving" />
        <TrafficQuotaFields v-model="guest" download-only :label="locale.text('访客共享额度', 'Shared guest allowance')" :disabled="saving" />
        <TrafficQuotaFields v-model="usersQuota" :label="locale.text('用户共享总额度', 'Shared user allowance')" :disabled="saving" />
        <fieldset class="cycle-fields">
          <legend>{{ locale.text('统一重置周期', 'Shared reset schedule') }}</legend>
          <div><label>{{ locale.text('每隔', 'Every') }}<input v-model.number="every" class="input" type="number" min="1" max="120" required></label><div class="cycle-unit"><span>{{ locale.text('周期单位', 'Unit') }}</span><AppSelect :model-value="unit" :label="locale.text('周期单位', 'Unit')" :options="[{ value: 'hours', label: locale.text('小时', 'Hours') }, { value: 'days', label: locale.text('天', 'Days') }, { value: 'months', label: locale.text('月', 'Months') }]" @update:model-value="unit = $event as typeof unit" /></div></div>
          <label>{{ locale.text('起始日期（本地日期）', 'Start date (local date)') }}<AppDatePicker v-model="anchor" placement="top" :show-footer="false" :label="locale.text('起始日期（本地日期）', 'Start date (local date)')" min="2000-01-01" max="2100-01-01" /></label>
          <small>{{ locale.text('访客、用户和全站统一重置；修改周期不会立即清除用量，历史统计保留。', 'Guests, users and site reset together. Changing the schedule does not clear usage or history.') }}</small>
        </fieldset>
        <div class="modal-actions"><button class="btn secondary" type="button" :disabled="saving" @click="editing = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="saving">{{ locale.text('确认', 'Confirm') }}</button></div>
      </form>
    </SettingsDrawer>
  </div>
</template>

<style scoped>
.traffic-panel { --download-used: #10b982; --upload-used: #6367f1; --traffic-track: color-mix(in srgb,var(--muted) 7%,var(--panel)); min-width: 0; }
.traffic-mode-dashboard { display: grid; gap: 10px; min-height: 0; }
.dashboard-block { min-width: 0; padding: 22px 24px; border-radius: 10px; }
.traffic-usage-block { min-width: 0; }
.traffic-usage-heading h2,.history-heading h2 { margin: 0; font-size: 17px; }
.traffic-meters { width: 100%; display: grid; grid-template-columns: repeat(5,minmax(112px,1fr)); align-items: start; gap: 10px; padding: 18px 0 0; }
.traffic-meter { min-width: 0; display: grid; justify-items: center; gap: 9px; padding: 4px 8px; text-align: center; }
.traffic-meter h3 { min-height: 21px; margin: 0; font-size: 13px; font-weight: 500; color: var(--muted); }
.usage-dial { position: relative; width: min(100%,122px); aspect-ratio: 1; }
.usage-dial svg { display: block; width: 100%; height: 100%; transform: rotate(-90deg); }
.ring-track,.ring-value { fill: none; stroke-width: 8; }
.ring-track { stroke: var(--traffic-track); }
.ring-value { stroke-linecap: round; }
.meter-download .ring-value { stroke: var(--download-used); }
.meter-upload .ring-value { stroke: var(--upload-used); }
.dial-center { position: absolute; inset: 19%; display: grid; place-content: center; }
.dial-center strong { font-size: 17px; font-weight: 600; line-height: 1.1; font-variant-numeric: tabular-nums; }
.meter-amount { margin: 0; color: var(--muted-2); font-size: 12px; font-variant-numeric: tabular-nums; white-space: nowrap; }
.traffic-note { margin: 7px 0 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
.history-heading { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 14px; margin-bottom: 18px; }
.range-controls { display: grid; justify-items: end; gap: 8px; }
.range-presets { display: inline-flex; padding: 3px; background: var(--panel-soft); border-radius: 7px; }
.range-presets button { min-width: 66px; height: 32px; padding: 0 12px; color: var(--muted); background: transparent; border: 0; border-radius: 5px; font-size: 14px; font-weight: 400; cursor: pointer; }
.range-presets button.active { color: var(--text); background: var(--panel); box-shadow: 0 0 0 1px var(--line); font-weight: 500; }
.traffic-range { display: flex; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: 8px; margin: 0; }
.traffic-range > span { color: var(--muted); font-size: 12px; }
.traffic-range :deep(.app-date-picker) { width: 136px; }
.traffic-range :deep(.date-picker-trigger) { height: 36px; }
.traffic-range .btn { min-height: 36px; height: 36px; }
.history-legend span { display: inline-flex; align-items: center; gap: 7px; padding: 6px 10px; color: var(--muted); background: var(--panel); border: 1px solid var(--line); border-radius: 5px; }
.history-legend i,.day-tooltip i { width: 8px; height: 8px; flex: 0 0 auto; border-radius: 2px; background: currentColor; }
.download-key { color: var(--download-used) !important; }
.upload-key { color: var(--upload-used) !important; }
.history-layout { display: grid; grid-template-columns: minmax(0,2fr) minmax(230px,1fr); gap: 16px; align-items: stretch; }
.trend-panel,.share-panel { min-width: 0; padding: 0; background: transparent; border: 0; border-radius: 0; }
.trend-panel h3,.share-panel h3 { margin: 0; color: var(--muted); font-size: 13px; font-weight: 500; }
.bar-chart { position: relative; display: flex; gap: 10px; height: 220px; margin-top: 18px; }
.chart-axis { display: flex; flex-direction: column; justify-content: space-between; width: 64px; padding-bottom: 22px; color: var(--muted); font-size: 10px; }
.chart-days { flex: 1; display: flex; min-width: 0; border-bottom: 1px solid var(--line); background: repeating-linear-gradient(to top,transparent,transparent calc(50% - 1px),var(--line) 50%); }
.chart-day { position: relative; flex: 1; min-width: 0; display: flex; flex-direction: column; }
.day-bars { display: flex; flex: 1; align-items: end; justify-content: center; gap: 3px; min-width: 0; padding: 0 2px; background: transparent; border: 0; border-radius: 0; cursor: pointer; }
.day-bars span { width: 34%; max-width: 16px; border-radius: 3px 3px 0 0; }
.day-bars .download { background: var(--download-used); }
.day-bars .upload { background: var(--upload-used); }
.day-bars:hover,.day-bars:focus-visible,.day-bars.selected { background: rgb(128 128 128 / .12); }
.chart-day small { height: 22px; padding-top: 7px; color: var(--muted); font-size: 10px; white-space: nowrap; }
.day-tooltip { position: absolute; z-index: 4; box-sizing: border-box; padding: 10px 12px; color: var(--muted); background: var(--panel); border: 1px solid var(--line); border-radius: 6px; box-shadow: 0 8px 24px rgb(15 23 42 / .14); pointer-events: none; }
.day-tooltip > strong { display: block; margin-bottom: 6px; color: var(--text); font-size: 12px; font-weight: 500; }
.day-tooltip span,.day-tooltip em { display: flex; align-items: center; gap: 6px; min-height: 22px; font-size: 12px; font-style: normal; }
.day-tooltip b { margin-left: auto; color: var(--text); font-weight: 600; }
.day-tooltip em { margin-top: 3px; padding-top: 5px; border-top: 1px solid var(--line); }
.share-panel { display: grid; align-content: start; justify-items: center; }
.share-panel h3 { justify-self: start; }
.share-donut { position: relative; width: min(78%,230px); aspect-ratio: 1; margin: 24px auto 10px; }
.share-donut > svg { width: 100%; height: 100%; overflow: visible; transform: rotate(-90deg); }
.share-ring-track,.share-ring { fill: none; stroke-width: 18; transform-box: fill-box; transform-origin: center; }
.share-ring-track { stroke: var(--traffic-track); }
.share-ring { transition: transform .18s ease, stroke-width .18s ease, filter .18s ease; }
.share-ring-download { stroke: var(--download-used); }
.share-ring-upload { stroke: var(--upload-used); }
.share-ring.active { stroke-width: 19; transform: scale(1.025); filter: drop-shadow(0 3px 4px rgb(15 23 42 / .12)); }
.share-donut > div { position: absolute; z-index: 1; inset: 26%; display: grid; place-content: center; gap: 6px; text-align: center; }
.share-donut strong { font-size: 20px; font-weight: 600; font-variant-numeric: tabular-nums; }
.share-donut span { color: var(--muted); font-size: 11px; }
.share-tooltip-slot { position: relative; width: 100%; height: 36px; }
.share-tooltip { position: absolute; z-index: 3; top: 0; width: max-content; max-width: min(100%,180px); margin: 0; padding: 7px 10px; transform: translateX(-50%); color: var(--text); background: var(--panel); border: 1px solid var(--line); border-radius: 5px; box-shadow: 0 8px 20px rgb(15 23 42 / .12); font-size: 11px; text-align: center; pointer-events: none; }
.history-legend { display: flex; flex-wrap: wrap; justify-content: center; gap: 8px; margin-top: 4px; font-size: 12px; }
.traffic-form { display: grid; gap: 24px; }
.cycle-fields { display: grid; gap: 16px; min-width: 0; margin: 0; padding: 20px 0 0; border: 0; border-top: 1px solid var(--line); }
.cycle-fields legend { padding: 0 8px 0 0; font-size: 14px; font-weight: 400; }
.cycle-fields label,.cycle-unit { min-width: 0; display: grid; gap: 8px; color: var(--muted); font-size: 14px; }
.cycle-fields :deep(.app-select-trigger) { height: 36px; min-height: 36px; border-radius: 3px; font-size: 14px; font-weight: 400; }
.cycle-fields > div { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
@media(max-width: 1050px) { .traffic-meters { grid-template-columns: repeat(5,minmax(100px,1fr)); overflow-x: auto; } }
@media(max-width: 800px) { .history-layout { grid-template-columns: 1fr; } .history-heading { display: grid; } .range-controls { justify-items: start; } .traffic-range { justify-content: start; } }
@media(max-width: 520px) { .traffic-meters { grid-template-columns: repeat(5,108px); padding-inline: 12px; } .chart-axis { width: 48px; } }
</style>

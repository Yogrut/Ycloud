<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import AppIcon from './AppIcon.vue'
import AppSelect from './AppSelect.vue'
import { useLocale } from '../i18n'

const props = withDefaults(defineProps<{
  modelValue: string
  label: string
  datetime?: boolean
  min?: string
  max?: string
  disabled?: boolean
  showFooter?: boolean
  placement?: 'auto' | 'top' | 'bottom'
}>(), { datetime: false, min: '', max: '', disabled: false, showFooter: true, placement: 'auto' })
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const locale = useLocale()
const root = ref<HTMLElement>()
const trigger = ref<HTMLButtonElement>()
const popover = ref<HTMLElement>()
const open = ref(false)
const resolvedPlacement = ref<'top' | 'bottom'>('bottom')
const popoverStyle = ref<Record<string, string>>({})
const cursor = ref(new Date())
const pad = (value: number) => String(value).padStart(2, '0')
const datePart = (value: string) => value.slice(0, 10)
const localToday = () => {
  const now = new Date()
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`
}
function parseDate(value: string): Date | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(value)
  if (!match) return undefined
  const result = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]))
  return Number.isNaN(result.getTime()) ? undefined : result
}
const display = computed(() => {
  if (!props.modelValue) return locale.text('请选择', 'Select')
  const date = parseDate(props.modelValue)
  if (!date) return props.modelValue
  const base = `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())}`
  return props.datetime ? `${base} ${props.modelValue.slice(11, 16) || '00:00'}` : base
})
const monthTitle = computed(() => locale.text(
  `${cursor.value.getFullYear()}年${pad(cursor.value.getMonth() + 1)}月`,
  cursor.value.toLocaleDateString('en', { month: 'long', year: 'numeric' }),
))
const weekdays = computed(() => locale.isEnglish.value
  ? ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']
  : ['一', '二', '三', '四', '五', '六', '日'])
const calendarDays = computed(() => {
  const first = new Date(cursor.value.getFullYear(), cursor.value.getMonth(), 1)
  const start = new Date(first)
  start.setDate(1 - ((first.getDay() + 6) % 7))
  return Array.from({ length: 42 }, (_, index) => {
    const date = new Date(start)
    date.setDate(start.getDate() + index)
    const value = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
    const min = datePart(props.min)
    const max = datePart(props.max)
    return {
      value,
      day: date.getDate(),
      current: date.getMonth() === cursor.value.getMonth(),
      disabled: Boolean((min && value < min) || (max && value > max)),
    }
  })
})
const hourOptions = Array.from({ length: 24 }, (_, value) => ({ value: pad(value), label: pad(value) }))
const minuteOptions = Array.from({ length: 60 }, (_, value) => ({ value: pad(value), label: pad(value) }))
const hour = computed({
  get: () => props.modelValue.slice(11, 13) || '00',
  set: value => updateTime(String(value), minute.value),
})
const minute = computed({
  get: () => props.modelValue.slice(14, 16) || '00',
  set: value => updateTime(hour.value, String(value)),
})
function emitDate(date: string, chosenHour = hour.value, chosenMinute = minute.value): void {
  emit('update:modelValue', props.datetime ? `${date}T${chosenHour}:${chosenMinute}` : date)
}
function updateTime(chosenHour: string, chosenMinute: string): void {
  emitDate(datePart(props.modelValue) || localToday(), chosenHour, chosenMinute)
}
function choose(value: string): void {
  emitDate(value)
  if (!props.datetime) hide(true)
}
function chooseToday(): void {
  const today = localToday()
  if ((datePart(props.min) && today < datePart(props.min)) || (datePart(props.max) && today > datePart(props.max))) return
  cursor.value = new Date()
  emitDate(today)
  if (!props.datetime) hide(true)
}
function changeMonth(amount: number): void {
  cursor.value = new Date(cursor.value.getFullYear(), cursor.value.getMonth() + amount, 1)
}
async function show(): Promise<void> {
  if (props.disabled) return
  cursor.value = parseDate(props.modelValue) ?? new Date()
  if (props.placement !== 'auto') resolvedPlacement.value = props.placement
  open.value = true
  await nextTick()
  positionPopover()
}
function hide(restore = false): void {
  open.value = false
  if (restore) nextTick(() => trigger.value?.focus({ preventScroll: true }))
}
function closeOutside(event: PointerEvent): void {
  const target = event.target as Node
  if (open.value && !root.value?.contains(target) && !popover.value?.contains(target)) hide()
}
function positionPopover(): void {
  if (!open.value || !trigger.value || !popover.value) return
  const triggerRect = trigger.value.getBoundingClientRect()
  const gap = 6
  const edge = 8
  const width = Math.min(292, window.innerWidth - edge * 2)
  const height = popover.value.offsetHeight
  const roomBelow = window.innerHeight - triggerRect.bottom
  const roomAbove = triggerRect.top
  const placement = props.placement === 'auto'
    ? (roomBelow >= height + gap || roomBelow >= roomAbove ? 'bottom' : 'top')
    : props.placement
  const idealTop = placement === 'top' ? triggerRect.top - height - gap : triggerRect.bottom + gap
  resolvedPlacement.value = placement
  popoverStyle.value = {
    left: `${Math.max(edge, Math.min(triggerRect.left, window.innerWidth - width - edge))}px`,
    top: `${Math.max(edge, Math.min(idealTop, window.innerHeight - height - edge))}px`,
    width: `${width}px`,
  }
}
function keydown(event: KeyboardEvent): void {
  if (event.key === 'Escape' && open.value) { event.preventDefault(); hide(true) }
}
onMounted(() => {
  document.addEventListener('pointerdown', closeOutside)
  document.addEventListener('keydown', keydown)
  window.addEventListener('resize', positionPopover)
  window.addEventListener('scroll', positionPopover, true)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', closeOutside)
  document.removeEventListener('keydown', keydown)
  window.removeEventListener('resize', positionPopover)
  window.removeEventListener('scroll', positionPopover, true)
})
</script>

<template>
  <div ref="root" class="app-date-picker" :class="{ open }">
    <button ref="trigger" class="input date-picker-trigger" type="button" :disabled="disabled" :aria-label="label" aria-haspopup="dialog" :aria-expanded="open" @click="open ? hide() : show()">
      <span>{{ display }}</span><AppIcon name="calendar" :size="16" />
    </button>
    <Teleport to="body">
      <div v-if="open" ref="popover" class="date-picker-popover" :class="`placement-${resolvedPlacement}`" :style="popoverStyle" role="dialog" :aria-label="label">
        <header><button type="button" :aria-label="locale.text('上个月', 'Previous month')" @click="changeMonth(-1)">‹</button><strong>{{ monthTitle }}</strong><button type="button" :aria-label="locale.text('下个月', 'Next month')" @click="changeMonth(1)">›</button></header>
        <div class="weekday-row"><span v-for="day in weekdays" :key="day">{{ day }}</span></div>
        <div class="calendar-grid">
          <button v-for="day in calendarDays" :key="day.value" type="button" :class="{ outside: !day.current, selected: datePart(modelValue) === day.value, today: localToday() === day.value }" :disabled="day.disabled" @click="choose(day.value)">{{ day.day }}</button>
        </div>
        <div v-if="datetime" class="time-row">
          <span>{{ locale.text('时间', 'Time') }}</span>
          <AppSelect v-model="hour" :label="locale.text('小时', 'Hour')" :options="hourOptions" placement="top" />
          <span>:</span>
          <AppSelect v-model="minute" :label="locale.text('分钟', 'Minute')" :options="minuteOptions" placement="top" />
        </div>
        <footer v-if="showFooter"><button type="button" @click="emit('update:modelValue', '')">{{ locale.text('清除', 'Clear') }}</button><button type="button" @click="chooseToday">{{ locale.text('今天', 'Today') }}</button><button v-if="datetime" class="done" type="button" @click="hide(true)">{{ locale.text('完成', 'Done') }}</button></footer>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.app-date-picker { position: relative; min-width: 0; }
.date-picker-trigger { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 10px; text-align: left; color: var(--text); background: var(--panel); }
.date-picker-popover { position: fixed; z-index: 180; width: 292px; padding: 12px; color: var(--text); background: var(--panel); border: 1px solid var(--line); border-radius: 6px; box-shadow: 0 12px 30px rgb(15 23 42 / .16); }
.date-picker-popover header { display: grid; grid-template-columns: 34px 1fr 34px; align-items: center; margin-bottom: 8px; }
.date-picker-popover header strong { text-align: center; font-size: 14px; }
.date-picker-popover button { border: 0; background: transparent; color: inherit; cursor: pointer; }
.date-picker-popover header button { height: 32px; border-radius: 4px; font-size: 23px; line-height: 1; }
.date-picker-popover button:hover,.date-picker-popover button:focus-visible { color: var(--accent); background: var(--accent-soft); outline: none; }
.weekday-row,.calendar-grid { display: grid; grid-template-columns: repeat(7, 1fr); }
.weekday-row span { padding: 6px 0; text-align: center; font-size: 11px; color: var(--muted); }
.calendar-grid button { height: 34px; border-radius: 4px; font-size: 12px; }
.calendar-grid button.outside { color: var(--muted); opacity: .58; }
.calendar-grid button.today { box-shadow: inset 0 0 0 1px color-mix(in srgb,var(--accent) 55%,var(--line)); }
.calendar-grid button.selected { color: #fff; background: var(--accent); }
.calendar-grid button:disabled { opacity: .25; cursor: not-allowed; }
.time-row { display: grid; grid-template-columns: 1fr 64px auto 64px; gap: 6px; align-items: center; padding: 10px 2px 4px; border-top: 1px solid var(--line); font-size: 12px; color: var(--muted); }
.time-row :deep(.app-select-trigger) { height: 32px; min-height: 32px; border-radius: 4px; }
.time-row :deep(.app-select-menu) { max-height: 190px; overflow: auto; }
.date-picker-popover footer { display: flex; align-items: center; gap: 4px; margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--line); }
.date-picker-popover footer button { min-width: 48px; height: 32px; padding: 0 10px; border-radius: 4px; color: var(--accent); }
.date-picker-popover footer .done { margin-left: auto; color: #fff; background: var(--accent); }
@media(max-width: 420px) { .date-picker-popover { width: min(292px,calc(100vw - 36px)); } }
</style>

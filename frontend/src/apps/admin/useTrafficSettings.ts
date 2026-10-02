import { ref } from 'vue'
import type { Ref } from 'vue'
import { saveTraffic } from '../../shared/api/admin'
import type { TrafficCycle, TrafficInfo, TrafficQuota, TrafficSettings } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { emptyTrafficQuota, sameTrafficQuota, validTrafficQuota } from './trafficQuota'

type EditableTrafficSettings = Pick<TrafficSettings, 'total' | 'guest' | 'users_total' | 'cycle'>

interface TrafficSettingsContext {
  info: Ref<TrafficInfo | undefined>
  refresh: () => Promise<void>
  feedback: (error: unknown) => void
  onSaved: () => void
}

const QUOTA_GROUPS = ['total', 'guest', 'users_total'] as const
const MAX_CYCLE_INTERVAL = 120
const MILLISECONDS_PER_MINUTE = 60_000

function sameCycle(current: TrafficCycle, previous: TrafficCycle | undefined): boolean {
  return previous !== undefined && current.unit === previous.unit && current.every === previous.every
    && current.anchor === previous.anchor && current.offset_minutes === previous.offset_minutes
}

export function useTrafficSettings(context: TrafficSettingsContext) {
  const locale = useLocale()
  const saving = ref(false)
  const editing = ref(false)
  const total = ref<TrafficQuota>(emptyTrafficQuota())
  const guest = ref<TrafficQuota>(emptyTrafficQuota())
  const usersQuota = ref<TrafficQuota>(emptyTrafficQuota())
  const unit = ref<TrafficCycle['unit']>('months')
  const every = ref(1)
  const anchor = ref('')
  let originalSettings: EditableTrafficSettings | undefined
  let originalAnchor = ''

  async function openEditor(): Promise<void> {
    if (!context.info.value) await context.refresh()
    if (!context.info.value) return
    const settings = context.info.value.settings
    // Only snapshot fields this editor can change; account quotas are separate.
    originalSettings = {
      total: { ...settings.total },
      guest: { ...settings.guest },
      users_total: { ...settings.users_total },
      cycle: { ...settings.cycle },
    }
    total.value = { ...settings.total }
    guest.value = { ...settings.guest }
    usersQuota.value = { ...settings.users_total }
    unit.value = settings.cycle.unit
    every.value = settings.cycle.every
    const date = new Date(settings.cycle.anchor * 1000)
    anchor.value = new Date(date.getTime() - date.getTimezoneOffset() * MILLISECONDS_PER_MINUTE).toISOString().slice(0, 10)
    originalAnchor = anchor.value
    editing.value = true
  }

  function settingsChanges(timestamp: Date): Partial<EditableTrafficSettings> {
    const changes: Partial<EditableTrafficSettings> = {}
    const quotas = { total: total.value, guest: guest.value, users_total: usersQuota.value }
    for (const key of QUOTA_GROUPS) {
      if (!sameTrafficQuota(quotas[key], originalSettings?.[key])) changes[key] = { ...quotas[key] }
    }
    // A date-only control cannot reproduce a stored time or timezone. Keep the
    // exact original cycle unless the administrator edits its controls.
    const cycle: TrafficCycle = originalSettings && unit.value === originalSettings.cycle.unit
      && every.value === originalSettings.cycle.every && anchor.value === originalAnchor
      ? originalSettings.cycle
      : {
          unit: unit.value,
          every: every.value,
          anchor: Math.floor(timestamp.getTime() / 1000),
          offset_minutes: -timestamp.getTimezoneOffset(),
        }
    if (!sameCycle(cycle, originalSettings?.cycle)) changes.cycle = { ...cycle }
    return changes
  }

  async function save(): Promise<void> {
    if (saving.value) return
    const timestamp = new Date(`${anchor.value}T00:00:00`)
    const invalidQuota = [total.value, guest.value, usersQuota.value].some(quota => !validTrafficQuota(quota))
    if (!Number.isFinite(timestamp.getTime()) || !Number.isInteger(every.value)
      || every.value < 1 || every.value > MAX_CYCLE_INTERVAL || invalidQuota) {
      context.feedback(new Error(locale.text('请填写有效的额度、周期和起始日期', 'Enter a valid allowance, interval and start date')))
      return
    }
    saving.value = true
    try {
      await saveTraffic(settingsChanges(timestamp))
      editing.value = false
      context.onSaved()
      await context.refresh()
    } catch (error) {
      context.feedback(error)
    } finally {
      saving.value = false
    }
  }

  return { total, guest, usersQuota, unit, every, anchor, editing, saving, openEditor, save }
}

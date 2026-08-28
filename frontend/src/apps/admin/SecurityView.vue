<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import type { AdminInfo, LoginEntry, LoginEvent, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'
import { AdminApiError, getLoginEvents, updateLoginRestriction, updateLoginSecuritySettings } from '../../shared/api/admin'
import AppSelect from '../../shared/components/AppSelect.vue'
import { useLocale } from '../../shared/i18n'

type RecordKind = 'normal' | 'error'
type RestrictionAction = 'block' | 'unblock'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()

const kind = ref<RecordKind>('normal')
const days = ref('7')
const entry = ref<LoginEntry | ''>('')
const ip = ref('')
const events = ref<LoginEvent[]>([])
const loading = ref(false)
const loadError = ref('')
const nextCursor = ref<number | null>(null)
const currentCursor = ref<number>()
const cursorHistory = ref<Array<number | undefined>>([])
const pending = ref<{ action: RestrictionAction; event: LoginEvent }>()
const submitting = ref(false)
const actionError = ref('')
const retentionDays = ref<string | number>(props.info.security_log_retention_days)
const maxEntries = ref<string | number>(props.info.security_log_max_entries)
const savingRetention = ref(false)
const retentionError = ref('')

const pageNumber = computed(() => cursorHistory.value.length + 1)
const dayOptions = computed(() => [1, 3, 7, 15, 30].map(value => ({
  value: String(value),
  label: value === 1 ? locale.text('最近 1 天', 'Last day') : locale.text(`最近 ${value} 天`, `Last ${value} days`),
})))
const entryOptions = computed(() => [
  { value: '', label: locale.text('全部', 'All') },
  { value: 'admin', label: locale.text('管理员', 'Administrator') },
  { value: 'account', label: locale.text('用户账号', 'User account') },
  { value: 'web', label: locale.text('首页', 'Browser') },
  { value: 'web_dav', label: 'WebDAV' },
])
const retentionOptions = computed(() => [1, 3, 5, 7, 15, 30].map(value => ({
  value,
  label: `${value} ${locale.text('天', 'days')}`,
})))

function entryLabel(value: LoginEntry): string {
  if (value === 'admin') return locale.text('管理员', 'Administrator')
  if (value === 'account') return locale.text('用户账号', 'User account')
  if (value === 'web') return locale.text('首页', 'Browser')
  return 'WebDAV'
}

function resultLabel(result: string): string {
  if (!locale.isEnglish.value) return result
  const translations: Record<string, string> = {
    '登录成功': 'Sign-in successful',
    '凭据错误': 'Invalid credentials',
    '凭据错误，已限制': 'Invalid credentials; restriction applied',
    '管理员已解除限制': 'Restriction removed by administrator',
    '管理员已限制': 'Restricted by administrator',
  }
  return translations[result] ?? result
}

function formatTime(timestamp: number | null): string {
  if (!timestamp) return '—'
  return new Intl.DateTimeFormat(locale.current.value, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
  }).format(new Date(timestamp * 1000))
}

async function load(cursor?: number): Promise<void> {
  loading.value = true
  loadError.value = ''
  try {
    const result = await getLoginEvents({
      success: kind.value === 'normal',
      since: Math.floor(Date.now() / 1000) - Number(days.value) * 24 * 60 * 60,
      entry: entry.value || undefined,
      ip: ip.value,
      cursor,
      limit: 10,
    })
    events.value = result.events
    nextCursor.value = result.next_cursor
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : locale.text('登录日志加载失败', 'Unable to load sign-in logs')
  } finally {
    loading.value = false
  }
}

function resetAndLoad(): void {
  currentCursor.value = undefined
  cursorHistory.value = []
  void load()
}

function nextPage(): void {
  if (nextCursor.value === null) return
  cursorHistory.value.push(currentCursor.value)
  currentCursor.value = nextCursor.value
  void load(currentCursor.value)
}

function previousPage(): void {
  if (!cursorHistory.value.length) return
  currentCursor.value = cursorHistory.value.pop()
  void load(currentCursor.value)
}

function ask(action: RestrictionAction, event: LoginEvent): void {
  pending.value = { action, event }
  actionError.value = ''
}

async function confirmAction(): Promise<void> {
  if (!pending.value || submitting.value) return
  submitting.value = true
  actionError.value = ''
  const { action, event } = pending.value
  try {
    await updateLoginRestriction(action, event.entry, event.ip)
    pending.value = undefined
    emit('changed', action === 'block'
      ? locale.text(`已限制 ${event.ip} 的${entryLabel(event.entry)}登录`, `Blocked ${event.ip} from ${entryLabel(event.entry)} sign-in`)
      : locale.text(`已解除 ${event.ip} 的${entryLabel(event.entry)}限制`, `Removed the ${entryLabel(event.entry)} restriction for ${event.ip}`))
  } catch (error) {
    actionError.value = error instanceof AdminApiError && error.status === 404
      ? locale.text('该 IP 当前没有可解除的限制', 'This IP has no active restriction to remove')
      : error instanceof Error ? error.message : locale.t('common.failed')
  } finally {
    submitting.value = false
  }
}

async function saveRetention(): Promise<void> {
  const retention = Number(retentionDays.value)
  const maximum = Number(maxEntries.value)
  if (![1, 3, 5, 7, 15, 30].includes(retention)) {
    retentionError.value = locale.text('请选择有效的日志保存天数', 'Choose a valid log retention period')
    return
  }
  if (!Number.isInteger(maximum) || maximum < 500 || maximum > 20000) {
    retentionError.value = locale.text('日志条目上限必须在 500 到 20000 之间', 'Log entry limit must be between 500 and 20,000')
    return
  }
  const body: UpdateLoginSecuritySettingsRequest = {
    security_log_retention_days: retention,
    security_log_max_entries: maximum,
  }
  savingRetention.value = true
  retentionError.value = ''
  try {
    await updateLoginSecuritySettings(body)
    emit('changed', locale.text('登录日志保存策略已更新', 'Sign-in log retention updated'))
  } catch (error) {
    retentionError.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    savingRetention.value = false
  }
}

watch([kind, days, entry], resetAndLoad)
watch(() => props.info, (value) => {
  retentionDays.value = value.security_log_retention_days
  maxEntries.value = value.security_log_max_entries
})
onMounted(() => load())
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="security-title">
    <header class="admin-pane-head security-head">
      <div>
        <h1 id="security-title">{{ locale.text('访问日志', 'Access logs') }}</h1>
        <p>{{ locale.text('逐次记录登录结果；WebDAV 成功认证按每 IP 每小时合并一次，避免文件请求淹没日志。', 'Each sign-in attempt is recorded. Successful WebDAV authentication is coalesced per IP per hour to prevent file requests from flooding the log.') }}</p>
      </div>
    </header>
    <div class="admin-pane-body security-body">
      <div class="security-tabs" role="tablist">
        <button :class="{ active: kind === 'normal' }" type="button" @click="kind = 'normal'">{{ locale.text('正常登录', 'Successful') }}</button>
        <button :class="{ active: kind === 'error' }" type="button" @click="kind = 'error'">{{ locale.text('错误登录', 'Failed') }}</button>
      </div>

      <div class="security-filters">
        <label><span>{{ locale.text('查看时间', 'Time range') }}</span><AppSelect v-model="days" :options="dayOptions" :label="locale.text('查看时间', 'Time range')" /></label>
        <label><span>{{ locale.text('登录入口', 'Entry') }}</span><AppSelect v-model="entry" :options="entryOptions" :label="locale.text('登录入口', 'Entry')" /></label>
        <label class="security-ip-filter"><span>IP</span><input v-model="ip" maxlength="64" :placeholder="locale.text('筛选 IP', 'Filter IP')" @keyup.enter="resetAndLoad"></label>
        <button class="btn secondary" type="button" @click="resetAndLoad">{{ locale.text('查询', 'Search') }}</button>
      </div>

      <p v-if="loadError" class="admin-form-error">{{ loadError }}</p>
      <div class="security-table-wrap">
        <table class="security-table">
          <thead><tr><th>{{ locale.text('登录结果', 'Result') }}</th><th>{{ locale.text('登录时间', 'Time') }}</th><th>{{ locale.text('入口', 'Entry') }}</th><th>{{ locale.text('请求 IP', 'Request IP') }}</th><th>UserAgent</th><th>{{ locale.text('状态', 'Status') }}</th><th>{{ locale.text('操作', 'Action') }}</th></tr></thead>
          <tbody>
            <tr v-if="loading"><td colspan="7">{{ locale.t('common.loading') }}</td></tr>
            <tr v-else-if="!events.length"><td colspan="7">{{ locale.text('当前筛选范围内没有日志', 'No logs match these filters') }}</td></tr>
            <tr v-for="event in events" v-else :key="event.id">
              <td>{{ resultLabel(event.result) }}</td>
              <td class="security-time">{{ formatTime(event.occurred_at) }}</td>
              <td>{{ entryLabel(event.entry) }}</td>
              <td class="security-ip">{{ event.ip }}</td>
              <td class="security-agent" :title="event.user_agent || ''">{{ event.user_agent || '—' }}</td>
              <td><span class="status-pill" :class="{ danger: event.current_blocked_until }">{{ event.current_blocked_until ? locale.text('已封禁', 'Blocked') : locale.text('正常', 'Normal') }}</span></td>
              <td class="security-operation"><button v-if="kind === 'normal' && !event.current_blocked_until" class="btn secondary" type="button" @click="ask('block', event)">{{ locale.text('封禁 IP', 'Block IP') }}</button><button v-else-if="kind === 'error' && event.current_blocked_until" class="btn secondary" type="button" @click="ask('unblock', event)">{{ locale.text('解除封禁', 'Unblock') }}</button><span v-else>—</span></td>
            </tr>
          </tbody>
        </table>
      </div>
      <nav class="security-pagination"><button class="btn secondary" type="button" :disabled="!cursorHistory.length || loading" @click="previousPage">{{ locale.text('上一页', 'Previous') }}</button><span>{{ locale.text(`第 ${pageNumber} 页`, `Page ${pageNumber}`) }}</span><button class="btn secondary" type="button" :disabled="nextCursor === null || loading" @click="nextPage">{{ locale.text('下一页', 'Next') }}</button></nav>

      <form class="security-retention" @submit.prevent="saveRetention">
        <label><span>{{ locale.text('日志保存时间', 'Log retention') }}</span><AppSelect v-model="retentionDays" :options="retentionOptions" :label="locale.text('日志保存时间', 'Log retention')" /></label>
        <label><span>{{ locale.text('最多保存条目', 'Maximum entries') }}</span><input v-model="maxEntries" type="number" min="500" max="20000" step="100"></label>
        <button class="btn" type="submit" :disabled="savingRetention">{{ savingRetention ? locale.t('common.saving') : locale.text('保存日志设置', 'Save log settings') }}</button>
        <p class="admin-form-error">{{ retentionError }}</p>
      </form>
    </div>
  </section>

  <div v-if="pending" class="overlay" @click.self="pending = undefined">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="security-confirm-title">
      <h2 id="security-confirm-title">{{ pending.action === 'block' ? locale.text('确认封禁 IP', 'Block IP?') : locale.text('确认解除封禁', 'Unblock IP?') }}</h2>
      <p>
        {{ pending.action === 'block'
          ? locale.text(`将限制 ${pending.event.ip} 的${entryLabel(pending.event.entry)}登录。共享出口或 NAT 下可能同时影响多台设备。`, `${pending.event.ip} will be blocked from ${entryLabel(pending.event.entry)} sign-in. Shared egress or NAT may affect multiple devices.`)
          : locale.text(`将清除 ${pending.event.ip} 在${entryLabel(pending.event.entry)}入口的失败次数和封禁状态。`, `Failure counts and the restriction for ${pending.event.ip} at ${entryLabel(pending.event.entry)} will be cleared.`) }}
      </p>
      <p class="modal-error">{{ actionError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="submitting" @click="pending = undefined">{{ locale.t('common.cancel') }}</button><button class="btn" :class="{ danger: pending.action === 'block' }" type="button" :disabled="submitting" @click="confirmAction">{{ submitting ? locale.text('处理中…', 'Working…') : locale.t('common.confirm') }}</button></div>
    </section>
  </div>
</template>

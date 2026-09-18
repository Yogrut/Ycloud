<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { AdminInfo, LoginEntry, LoginEvent, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'
import { AdminApiError, clearLoginEvents, getLoginEvents, updateLoginRestriction, updateLoginSecuritySettings } from '../../shared/api/admin'
import AppSelect from '../../shared/components/AppSelect.vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import { useLocale } from '../../shared/i18n'

type RecordKind = '' | 'normal' | 'error'
type RestrictionAction = 'block' | 'unblock'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)

const kind = ref<RecordKind>('')
const entry = ref<LoginEntry | ''>('')
const ip = ref('')
const queryText = ref('')
const events = ref<LoginEvent[]>([])
const loading = ref(false)
const loadError = ref('')
const pageNumber = ref(1)
const pageSize = ref(20)
const total = ref(0)
const jumpPage = ref<string | number>(1)
const refreshSeconds = ref(0)
const retentionOpen = ref(false)
const clearOpen = ref(false)
const clearing = ref(false)
const clearError = ref('')
let requestVersion = 0
let timer: ReturnType<typeof setInterval> | undefined
const pending = ref<{ action: RestrictionAction; event: LoginEvent }>()
const submitting = ref(false)
const actionError = ref('')
const retentionDays = ref<string | number>(props.info.security_log_retention_days)
const maxEntries = ref<string | number>(props.info.security_log_max_entries)
const savingRetention = ref(false)
const retentionError = ref('')

const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)))
const pageOptions = computed(() => [20, 50, 100].map(value => ({ value, label: locale.text(`${value}条/页`, `${value}/page`) })))
const statusOptions = computed(() => [
  { value: '', label: locale.text('全部状态', 'All statuses') },
  { value: 'normal', label: locale.text('成功', 'Successful') },
  { value: 'error', label: locale.text('失败', 'Failed') },
])
const refreshOptions = computed(() => [0, 30, 60, 300].map(value => ({ value, label: value ? `${value}s` : locale.text('不刷新', 'No auto-refresh') })))
const visiblePages = computed(() => {
  const pages = [...new Set([1, ...Array.from({ length: 5 }, (_, i) => pageNumber.value - 2 + i), totalPages.value])].filter(p => p >= 1 && p <= totalPages.value).sort((a, b) => a - b)
  return pages.flatMap((p, i) => i && p - pages[i - 1]! > 1 ? ['…', p] : [p])
})
const entryOptions = computed(() => [
  { value: '', label: locale.text('全部入口', 'All entries') },
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

async function load(page = pageNumber.value): Promise<void> {
  const version = ++requestVersion
  loading.value = true
  loadError.value = ''
  try {
    const result = await getLoginEvents({
      success: kind.value ? kind.value === 'normal' : undefined,
      entry: entry.value || undefined,
      search: queryText.value,
      page,
      limit: pageSize.value,
    })
    if (version !== requestVersion) return
    events.value = result.events
    total.value = result.total
    pageNumber.value = result.page
    jumpPage.value = result.page
  } catch (error) {
    if (version === requestVersion) loadError.value = error instanceof Error ? error.message : locale.text('登录日志加载失败', 'Unable to load sign-in logs')
  } finally {
    if (version === requestVersion) loading.value = false
  }
}

function resetAndLoad(): void {
  queryText.value = ip.value.trim()
  void load(1)
}

function openRetention(): void {
  retentionDays.value = props.info.security_log_retention_days
  maxEntries.value = props.info.security_log_max_entries
  retentionError.value = ''
  retentionOpen.value = true
}

function goToPage(page: number): void {
  if (loading.value || !Number.isInteger(page) || page < 1 || page > totalPages.value) return
  void load(page)
}

async function clearLogs(): Promise<void> {
  if (clearing.value) return
  clearing.value = true
  clearError.value = ''
  ++requestVersion
  try {
    await clearLoginEvents()
    clearOpen.value = false
    await load(1)
  } catch (error) {
    clearError.value = error instanceof Error ? error.message : locale.t('common.failed')
  } finally { clearing.value = false }
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
    await load()
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
    retentionOpen.value = false
    await load(1)
    emit('changed', locale.text('登录日志保存策略已更新', 'Sign-in log retention updated'))
  } catch (error) {
    retentionError.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    savingRetention.value = false
  }
}

watch([kind, entry, pageSize], resetAndLoad)
watch(refreshSeconds, value => {
  clearInterval(timer)
  if (value) timer = setInterval(() => {
    if (!document.hidden && !loading.value && !clearOpen.value && !retentionOpen.value && !pending.value && pageNumber.value === 1) void load(1)
  }, value * 1000)
})
onBeforeUnmount(() => { clearInterval(timer); ++requestVersion })
watch(() => props.info, (value) => {
  retentionDays.value = value.security_log_retention_days
  maxEntries.value = value.security_log_max_entries
})
onMounted(() => load(1))
</script>

<template>
  <section class="admin-pane glass log-pane" aria-labelledby="security-title">
    <h1 id="security-title" class="visually-hidden">{{ locale.text('访问日志', 'Access logs') }}</h1>
    <div class="log-toolbar">
      <button class="btn secondary log-clear" type="button" :disabled="loading || clearing" @click="clearError = ''; clearOpen = true">{{ locale.text('清空日志', 'Clear logs') }}</button>
      <div class="log-filters">
        <AppSelect v-model="entry" :options="entryOptions" :label="locale.text('登录入口', 'Entry')" />
        <AppSelect v-model="kind" class="log-status-filter" :options="statusOptions" :label="locale.text('状态', 'Status')" />
        <form class="log-search" @submit.prevent="feedbackRevision++; resetAndLoad()"><input v-model="ip" maxlength="128" :aria-label="locale.text('搜索日志', 'Search logs')" :placeholder="locale.text('搜索 IP、用户代理、结果', 'Search IP, user agent, result')"><button type="submit" :aria-label="locale.text('搜索', 'Search')"><AppIcon name="search" :size="17" /></button></form>
        <button class="btn secondary log-refresh" type="button" :disabled="loading" :aria-label="locale.text('刷新', 'Refresh')" @click="resetAndLoad"><AppIcon name="retry" :size="17" /></button>
        <AppSelect v-model="refreshSeconds" class="log-auto-refresh" :options="refreshOptions" :label="locale.text('自动刷新', 'Auto-refresh')" />
        <button class="btn secondary log-refresh" type="button" :aria-label="locale.text('日志设置', 'Log settings')" @click="openRetention"><AppIcon name="settings" :size="17" /></button>
      </div>
    </div>
    <AppFeedback :revision="feedbackRevision" :message="loadError" />
    <div class="security-table-wrap">
      <table class="security-table">
        <thead><tr><th>{{ locale.text('登录 IP', 'Sign-in IP') }}</th><th>{{ locale.text('登录入口', 'Entry') }}</th><th>{{ locale.text('用户代理', 'User agent') }}</th><th>{{ locale.text('登录状态', 'Result') }}</th><th>{{ locale.text('时间', 'Time') }}</th><th>{{ locale.text('操作', 'Action') }}</th></tr></thead>
        <tbody>
          <tr v-if="loading"><td colspan="6">{{ locale.t('common.loading') }}</td></tr>
          <tr v-else-if="!events.length"><td colspan="6">{{ locale.text('当前筛选范围内没有日志', 'No logs match these filters') }}</td></tr>
          <tr v-for="event in events" v-else :key="event.id">
            <td class="security-ip">{{ event.ip }}</td>
            <td>{{ entryLabel(event.entry) }}</td>
            <td class="security-agent" :title="event.user_agent || ''">{{ event.user_agent || '—' }}</td>
            <td :title="resultLabel(event.result)"><span class="status-pill" :class="{ danger: !event.success }">{{ event.success ? locale.text('成功', 'Success') : locale.text('失败', 'Failed') }}</span><span v-if="event.current_blocked_until" class="status-pill danger">{{ locale.text('已封禁', 'Blocked') }}</span></td>
            <td class="security-time">{{ formatTime(event.occurred_at) }}</td>
            <td class="security-operation"><button class="log-text-action" type="button" @click="ask(event.current_blocked_until ? 'unblock' : 'block', event)">{{ event.current_blocked_until ? locale.text('解除封禁', 'Unblock') : locale.text('封禁 IP', 'Block IP') }}</button></td>
          </tr>
        </tbody>
      </table>
    </div>
    <nav class="log-pagination" :aria-label="locale.text('日志分页', 'Log pagination')">
      <span class="log-total">{{ locale.text(`共 ${total} 条`, `${total} records`) }}</span>
      <AppSelect v-model="pageSize" class="log-page-size" placement="top" :options="pageOptions" :label="locale.text('每页条数', 'Rows per page')" />
      <button type="button" :aria-label="locale.text('上一页', 'Previous')" :disabled="pageNumber <= 1 || loading" @click="goToPage(pageNumber - 1)">‹</button>
      <template v-for="(page, index) in visiblePages" :key="index"><span v-if="page === '…'">…</span><button v-else type="button" :class="{ active: page === pageNumber }" :aria-current="page === pageNumber ? 'page' : undefined" :disabled="loading" @click="goToPage(Number(page))">{{ page }}</button></template>
      <button type="button" :aria-label="locale.text('下一页', 'Next')" :disabled="pageNumber >= totalPages || loading" @click="goToPage(pageNumber + 1)">›</button>
      <form class="log-page-jump" @submit.prevent="feedbackRevision++; goToPage(Number(jumpPage))"><label>{{ locale.text('前往', 'Go to') }}<input v-model="jumpPage" type="number" min="1" :max="totalPages" :aria-label="locale.text('跳转页码', 'Page number')" @change="goToPage(Number(jumpPage))">{{ locale.text('页', 'page') }}</label></form>
    </nav>
  </section>

  <SettingsDrawer v-if="retentionOpen" :title="locale.text('日志设置', 'Log settings')" :busy="savingRetention" @close="retentionOpen = false">
    <form class="drawer-form" @submit.prevent="feedbackRevision++; saveRetention()">
      <label class="required-field">{{ locale.text('日志保存时间', 'Log retention') }}<AppSelect v-model="retentionDays" :options="retentionOptions" :label="locale.text('日志保存时间', 'Log retention')" /></label>
      <label>{{ locale.text('最多保存条目', 'Maximum entries') }}<input v-model="maxEntries" aria-required="true" class="input" type="number" min="500" max="20000" step="100"></label>
      <p class="field-hint">{{ locale.text('最多 20,000 条；超出保存时间或条数后自动清理。', 'Up to 20,000 entries. Older and excess records are removed automatically.') }}</p>
      <AppFeedback :revision="feedbackRevision" :message="retentionError" />
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="savingRetention" @click="retentionOpen = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="savingRetention">{{ locale.text('确认', 'Confirm') }}</button></div>
    </form>
  </SettingsDrawer>
  <ConfirmDialog
    v-if="clearOpen"
    :title="locale.text('清空全部登录日志？', 'Clear all sign-in logs?')"
    :message="locale.text('删除所有入口的登录日志，不仅是当前筛选结果。现有封禁和登录保护不受影响，删除后不可恢复。', 'Deletes all sign-in logs, not just filtered results. Restrictions and sign-in protection are unchanged. This cannot be undone.')"
    :error="clearError" :busy="clearing" :danger="true" :confirm-label="locale.text('清空日志', 'Clear logs')" @close="clearOpen = false" @confirm="clearLogs"
  />
  <ConfirmDialog
    v-if="pending"
    :title="pending.action === 'block' ? locale.text('确认封禁 IP', 'Block IP?') : locale.text('确认解除封禁', 'Unblock IP?')"
    :message="pending.action === 'block'
      ? locale.text(`将限制 ${pending.event.ip} 的${entryLabel(pending.event.entry)}登录。共享出口或 NAT 下可能同时影响多台设备。`, `${pending.event.ip} will be blocked from ${entryLabel(pending.event.entry)} sign-in. Shared egress or NAT may affect multiple devices.`)
      : locale.text(`将清除 ${pending.event.ip} 在${entryLabel(pending.event.entry)}入口的失败次数和封禁状态。`, `Failure counts and the restriction for ${pending.event.ip} at ${entryLabel(pending.event.entry)} will be cleared.`)"
    :error="actionError" :busy="submitting" @close="pending = undefined" @confirm="confirmAction"
  />
</template>

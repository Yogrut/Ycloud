<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { LoginRecord } from '../../shared/api/admin'
import { AdminApiError, updateLoginRestriction } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

type RecordKind = 'normal' | 'error'
type RestrictionAction = 'block' | 'unblock'

const PAGE_SIZE = 10
const props = defineProps<{ records: LoginRecord[] }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()

const kind = ref<RecordKind>('normal')
const days = ref('7')
const page = ref(1)
const pending = ref<{ action: RestrictionAction; record: LoginRecord }>()
const submitting = ref(false)
const errorMessage = ref('')

const now = computed(() => Math.floor(Date.now() / 1000))

function isBlocked(record: LoginRecord): boolean {
  return record.blocked_until !== null && record.blocked_until > now.value
}

function isErrorRecord(record: LoginRecord): boolean {
  return record.failed_attempts > 0 || isBlocked(record) || record.last_result !== '登录成功'
}

const recordsInRange = computed(() => {
  if (days.value === 'all') return props.records
  const since = now.value - Number(days.value) * 24 * 60 * 60
  return props.records.filter(record => record.last_attempt_at >= since)
})

const normalCount = computed(() => recordsInRange.value.filter(record => !isErrorRecord(record)).length)
const errorCount = computed(() => recordsInRange.value.filter(isErrorRecord).length)
const filtered = computed(() => recordsInRange.value.filter(record => kind.value === 'error' ? isErrorRecord(record) : !isErrorRecord(record)))
const pageCount = computed(() => Math.max(1, Math.ceil(filtered.value.length / PAGE_SIZE)))
const visible = computed(() => filtered.value.slice((page.value - 1) * PAGE_SIZE, page.value * PAGE_SIZE))

watch([kind, days], () => { page.value = 1 })
watch(pageCount, value => { if (page.value > value) page.value = value })

function entryLabel(entry: LoginRecord['entry']): string {
  if (entry === 'admin') return locale.text('管理员', 'Administrator')
  if (entry === 'web') return locale.text('首页', 'Browser')
  return 'WebDAV'
}

function resultLabel(result: string): string {
  if (!locale.isEnglish.value) return result
  const translations: Record<string, string> = {
    '登录成功': 'Sign-in successful',
    '凭据错误': 'Invalid credentials',
    '凭据错误，已限制': 'Invalid credentials; restriction applied',
    '限制已到期': 'Restriction expired',
    '管理员已解除限制': 'Restriction removed by administrator',
    '管理员已限制': 'Restricted by administrator',
  }
  return translations[result] ?? result
}

function formatTime(timestamp: number | null): string {
  if (!timestamp) return '—'
  return new Intl.DateTimeFormat(locale.current.value, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit',
    hour12: false,
  }).format(new Date(timestamp * 1000))
}

function ask(action: RestrictionAction, record: LoginRecord): void {
  pending.value = { action, record }
  errorMessage.value = ''
}

async function confirmAction(): Promise<void> {
  if (!pending.value || submitting.value) return
  submitting.value = true
  errorMessage.value = ''
  const { action, record } = pending.value
  try {
    await updateLoginRestriction(action, record.entry, record.ip)
    pending.value = undefined
    if (action === 'block') {
      kind.value = 'error'
      page.value = 1
    }
    emit('changed', action === 'block'
      ? locale.text(`已限制 ${record.ip} 的${entryLabel(record.entry)}登录`, `Blocked ${record.ip} from ${entryLabel(record.entry)} sign-in`)
      : locale.text(`已解除 ${record.ip} 的${entryLabel(record.entry)}限制`, `Removed the ${entryLabel(record.entry)} restriction for ${record.ip}`))
  } catch (error) {
    errorMessage.value = error instanceof AdminApiError && error.status === 404
      ? locale.text('当前运行的后端尚未加载 IP 封禁接口，请重启 Ycloud 后端后重试', 'The running backend does not expose the IP restriction endpoint. Restart the Ycloud backend and try again')
      : error instanceof Error ? error.message : locale.t('common.failed')
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="security-title">
    <header class="admin-pane-head security-head">
      <div>
        <h1 id="security-title">{{ locale.text('登录安全', 'Sign-in security') }}</h1>
        <p>{{ locale.text('每个客户端 IP 与登录入口保留一条最新状态，记录由服务器持久化并限制总量。', 'The server keeps the latest state for each client IP and sign-in entry, with persistent bounded storage.') }}</p>
      </div>
      <label class="security-range">
        <span>{{ locale.text('查看时间', 'Time range') }}</span>
        <select v-model="days">
          <option value="1">{{ locale.text('最近 1 天', 'Last 1 day') }}</option>
          <option value="3">{{ locale.text('最近 3 天', 'Last 3 days') }}</option>
          <option value="7">{{ locale.text('最近 7 天', 'Last 7 days') }}</option>
          <option value="15">{{ locale.text('最近 15 天', 'Last 15 days') }}</option>
          <option value="30">{{ locale.text('最近 30 天', 'Last 30 days') }}</option>
          <option value="all">{{ locale.text('全部记录', 'All records') }}</option>
        </select>
      </label>
    </header>
    <div class="admin-pane-body security-body">
      <div class="security-tabs" role="tablist" :aria-label="locale.text('登录记录分类', 'Sign-in record categories')">
        <button :class="{ active: kind === 'normal' }" type="button" role="tab" :aria-selected="kind === 'normal'" @click="kind = 'normal'">{{ locale.text('正常记录', 'Normal') }} <span>{{ normalCount }}</span></button>
        <button :class="{ active: kind === 'error' }" type="button" role="tab" :aria-selected="kind === 'error'" @click="kind = 'error'">{{ locale.text('错误与限制', 'Errors & restrictions') }} <span>{{ errorCount }}</span></button>
      </div>

      <div v-if="!visible.length" class="security-empty">{{ kind === 'normal' ? locale.text('当前范围内没有正常记录', 'No normal records in this range') : locale.text('当前范围内没有错误或限制记录', 'No errors or restrictions in this range') }}</div>
      <div v-else class="security-list">
        <article v-for="record in visible" :key="`${record.entry}:${record.ip}`" class="security-record">
          <div class="security-record-main">
            <div class="security-record-title">
              <strong>{{ record.ip }}</strong>
              <span>{{ entryLabel(record.entry) }}</span>
              <span class="status-pill" :class="{ danger: isErrorRecord(record) }">{{ isBlocked(record) ? locale.text('已限制', 'Blocked') : (isErrorRecord(record) ? locale.text('异常', 'Warning') : locale.text('正常', 'Normal')) }}</span>
            </div>
            <div class="security-record-meta">{{ locale.text('结果', 'Result') }}: {{ resultLabel(record.last_result) }} · {{ locale.text('失败', 'Failures') }} {{ record.failed_attempts }} · {{ locale.text('最后尝试', 'Last attempt') }} {{ formatTime(record.last_attempt_at) }}</div>
            <div class="security-record-meta">{{ locale.text('最后成功', 'Last success') }} {{ formatTime(record.last_success_at) }}<template v-if="isBlocked(record)"> · {{ locale.text('限制至', 'Blocked until') }} {{ formatTime(record.blocked_until) }}</template></div>
            <div class="security-user-agent">{{ record.user_agent || locale.text('未提供浏览器信息', 'No user-agent information') }}</div>
          </div>
          <button v-if="kind === 'normal'" class="btn secondary security-action" type="button" :aria-label="locale.text(`封禁 ${record.ip}`, `Block ${record.ip}`)" @click="ask('block', record)">{{ locale.text('封禁此 IP', 'Block this IP') }}</button>
          <button v-else class="btn secondary security-action" type="button" @click="ask('unblock', record)">{{ locale.text('解除限制', 'Remove restriction') }}</button>
        </article>
      </div>

      <nav v-if="filtered.length > PAGE_SIZE" class="security-pagination" :aria-label="locale.text('登录安全分页', 'Sign-in security pages')">
        <button class="btn secondary" type="button" :disabled="page <= 1" @click="page--">{{ locale.text('上一页', 'Previous') }}</button>
        <span>{{ locale.text(`第 ${page} / ${pageCount} 页`, `Page ${page} of ${pageCount}`) }}</span>
        <button class="btn secondary" type="button" :disabled="page >= pageCount" @click="page++">{{ locale.text('下一页', 'Next') }}</button>
      </nav>
    </div>
  </section>

  <div v-if="pending" class="overlay" @click.self="pending = undefined">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="security-confirm-title">
      <h2 id="security-confirm-title">{{ pending.action === 'block' ? locale.text('确认封禁 IP', 'Block IP?') : locale.text('确认解除限制', 'Remove restriction?') }}</h2>
      <p>
        {{ pending.action === 'block'
          ? locale.text(`将限制 ${pending.record.ip} 的${entryLabel(pending.record.entry)}登录，时长采用该入口现有安全策略。`, `${pending.record.ip} will be blocked from ${entryLabel(pending.record.entry)} sign-in for the duration configured by that entry's security policy.`)
          : locale.text(`将清除 ${pending.record.ip} 在${entryLabel(pending.record.entry)}入口的失败次数和限制状态。`, `Failure counts and restriction state for ${pending.record.ip} at the ${entryLabel(pending.record.entry)} entry will be cleared.`) }}
      </p>
      <p class="modal-error">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="submitting" @click="pending = undefined">{{ locale.t('common.cancel') }}</button>
        <button class="btn" :class="{ danger: pending.action === 'block' }" type="button" :disabled="submitting" @click="confirmAction">{{ submitting ? locale.text('处理中…', 'Working…') : locale.t('common.confirm') }}</button>
      </div>
    </section>
  </div>
</template>

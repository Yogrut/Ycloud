<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { LoginRecord } from '../../shared/api/admin'
import { updateLoginRestriction } from '../../shared/api/admin'

type RecordKind = 'normal' | 'error'
type RestrictionAction = 'block' | 'unblock'

const PAGE_SIZE = 10
const props = defineProps<{ records: LoginRecord[] }>()
const emit = defineEmits<{ changed: [message: string] }>()

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
  if (entry === 'admin') return '管理员'
  if (entry === 'web') return '首页'
  return 'WebDAV'
}

function formatTime(timestamp: number | null): string {
  if (!timestamp) return '—'
  return new Intl.DateTimeFormat('zh-CN', {
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
    emit('changed', action === 'block' ? `已限制 ${record.ip} 的${entryLabel(record.entry)}登录` : `已解除 ${record.ip} 的${entryLabel(record.entry)}限制`)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '操作失败'
  } finally {
    submitting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="security-title">
    <header class="admin-pane-head security-head">
      <div>
        <h1 id="security-title">登录安全</h1>
        <p>每个客户端 IP 与登录入口保留一条最新状态，记录由服务器持久化并限制总量。</p>
      </div>
      <label class="security-range">
        <span>查看时间</span>
        <select v-model="days">
          <option value="1">最近 1 天</option>
          <option value="3">最近 3 天</option>
          <option value="7">最近 7 天</option>
          <option value="15">最近 15 天</option>
          <option value="30">最近 30 天</option>
          <option value="all">全部记录</option>
        </select>
      </label>
    </header>
    <div class="admin-pane-body security-body">
      <div class="security-tabs" role="tablist" aria-label="登录记录分类">
        <button :class="{ active: kind === 'normal' }" type="button" role="tab" :aria-selected="kind === 'normal'" @click="kind = 'normal'">正常记录 <span>{{ normalCount }}</span></button>
        <button :class="{ active: kind === 'error' }" type="button" role="tab" :aria-selected="kind === 'error'" @click="kind = 'error'">错误与限制 <span>{{ errorCount }}</span></button>
      </div>

      <div v-if="!visible.length" class="security-empty">当前范围内没有{{ kind === 'normal' ? '正常' : '错误或限制' }}记录</div>
      <div v-else class="security-list">
        <article v-for="record in visible" :key="`${record.entry}:${record.ip}`" class="security-record">
          <div class="security-record-main">
            <div class="security-record-title">
              <strong>{{ record.ip }}</strong>
              <span>{{ entryLabel(record.entry) }}</span>
              <span class="status-pill" :class="{ danger: isErrorRecord(record) }">{{ isBlocked(record) ? '已限制' : (isErrorRecord(record) ? '异常' : '正常') }}</span>
            </div>
            <div class="security-record-meta">结果：{{ record.last_result }} · 失败 {{ record.failed_attempts }} 次 · 最后尝试 {{ formatTime(record.last_attempt_at) }}</div>
            <div class="security-record-meta">最后成功 {{ formatTime(record.last_success_at) }}<template v-if="isBlocked(record)"> · 限制至 {{ formatTime(record.blocked_until) }}</template></div>
            <div class="security-user-agent">{{ record.user_agent || '未提供浏览器信息' }}</div>
          </div>
          <button v-if="kind === 'normal'" class="btn secondary security-action" type="button" @click="ask('block', record)">封禁 IP</button>
          <button v-else class="btn secondary security-action" type="button" @click="ask('unblock', record)">解除限制</button>
        </article>
      </div>

      <nav v-if="filtered.length > PAGE_SIZE" class="security-pagination" aria-label="登录安全分页">
        <button class="btn secondary" type="button" :disabled="page <= 1" @click="page--">上一页</button>
        <span>第 {{ page }} / {{ pageCount }} 页</span>
        <button class="btn secondary" type="button" :disabled="page >= pageCount" @click="page++">下一页</button>
      </nav>
    </div>
  </section>

  <div v-if="pending" class="overlay" @click.self="pending = undefined">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="security-confirm-title">
      <h2 id="security-confirm-title">{{ pending.action === 'block' ? '确认封禁 IP' : '确认解除限制' }}</h2>
      <p>
        {{ pending.action === 'block'
          ? `将限制 ${pending.record.ip} 的${entryLabel(pending.record.entry)}登录，时长采用该入口现有安全策略。`
          : `将清除 ${pending.record.ip} 在${entryLabel(pending.record.entry)}入口的失败次数和限制状态。` }}
      </p>
      <p class="modal-error">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="submitting" @click="pending = undefined">取消</button>
        <button class="btn" :class="{ danger: pending.action === 'block' }" type="button" :disabled="submitting" @click="confirmAction">{{ submitting ? '处理中…' : '确定' }}</button>
      </div>
    </section>
  </div>
</template>

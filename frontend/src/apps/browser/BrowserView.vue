<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { adminLogin, listFiles, logout, unlockFolder } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import FileIcon from './FileIcon.vue'

type SortKey = 'name' | 'time' | 'size'

defineProps<{ theme: ThemeController }>()
const path = ref('')
const entries = ref<FileEntry[]>([])
const query = ref('')
const sort = ref<SortKey>('name')
const ascending = ref(true)
const selected = ref(new Set<string>())
const loading = ref(true)
const canWrite = ref(false)
const truncated = ref(false)
const notice = ref('')
const showUnlock = ref(false)
const unlockPath = ref('')
const unlockPassword = ref('')
const unlockError = ref('')
const showAdmin = ref(false)
const adminUser = ref('')
const adminPassword = ref('')
const adminError = ref('')

const visibleEntries = computed(() => {
  const term = query.value.trim().toLocaleLowerCase()
  const filtered = term ? entries.value.filter(entry => entry.name.toLocaleLowerCase().includes(term)) : entries.value
  const direction = ascending.value ? 1 : -1
  return [...filtered].sort((left, right) => {
    if (left.is_dir !== right.is_dir) return left.is_dir ? -1 : 1
    let a: string | number
    let b: string | number
    if (sort.value === 'time') {
      a = left.modified
      b = right.modified
    } else if (sort.value === 'size') {
      a = left.is_dir ? -1 : left.size
      b = right.is_dir ? -1 : right.size
    } else {
      a = left.name.toLocaleLowerCase()
      b = right.name.toLocaleLowerCase()
    }
    return a < b ? -direction : a > b ? direction : 0
  })
})

const crumbs = computed(() => {
  let accumulated = ''
  return path.value.split('/').filter(Boolean).map(label => {
    accumulated = accumulated ? `${accumulated}/${label}` : label
    return { label, path: accumulated }
  })
})

const allSelected = computed(() => visibleEntries.value.length > 0 && visibleEntries.value.every(entry => selected.value.has(entry.path)))

function announce(message: string): void {
  notice.value = message
  window.setTimeout(() => { if (notice.value === message) notice.value = '' }, 2800)
}

async function refresh(): Promise<void> {
  loading.value = true
  try {
    const data = await listFiles(path.value)
    path.value = data.current_path.replace(/^\/+|\/+$/g, '')
    entries.value = data.entries
    canWrite.value = data.can_write
    truncated.value = data.truncated
    selected.value = new Set()
    if (data.truncated) announce('目录内容超过显示上限，当前仅显示部分项目')
  } catch (error) {
    announce(error instanceof Error ? error.message : '目录加载失败')
  } finally {
    loading.value = false
  }
}

async function navigate(destination: string): Promise<void> {
  path.value = destination.replace(/^\/+|\/+$/g, '')
  query.value = ''
  await refresh()
}

function changeSort(key: SortKey): void {
  if (sort.value === key) ascending.value = !ascending.value
  else {
    sort.value = key
    ascending.value = true
  }
}

function toggleSelection(entryPath: string): void {
  const next = new Set(selected.value)
  if (next.has(entryPath)) next.delete(entryPath)
  else next.add(entryPath)
  selected.value = next
}

function toggleSelectAll(): void {
  const next = new Set(selected.value)
  if (allSelected.value) visibleEntries.value.forEach(entry => next.delete(entry.path))
  else visibleEntries.value.forEach(entry => next.add(entry.path))
  selected.value = next
}

function openEntry(entry: FileEntry): void {
  if (entry.is_dir) {
    if (entry.locked) {
      unlockPath.value = entry.path
      unlockPassword.value = ''
      unlockError.value = ''
      showUnlock.value = true
    } else void navigate(entry.path)
    return
  }
  window.open(`/preview.html?path=${encodeURIComponent(`/${entry.path}`)}`, '_blank', 'noopener')
}

async function submitUnlock(): Promise<void> {
  if (!unlockPassword.value) return
  try {
    const result = await unlockFolder(unlockPath.value, unlockPassword.value)
    if (!result.success) throw new Error(result.message ?? '密码错误')
    showUnlock.value = false
    await navigate(unlockPath.value)
  } catch (error) {
    unlockError.value = error instanceof Error ? error.message : '解锁失败'
  }
}

async function openAdmin(): Promise<void> {
  if (canWrite.value) {
    window.location.href = '/admin'
    return
  }
  adminUser.value = ''
  adminPassword.value = ''
  adminError.value = ''
  showAdmin.value = true
}

async function submitAdmin(): Promise<void> {
  if (!adminUser.value.trim() || !adminPassword.value) {
    adminError.value = '请输入用户名和密码'
    return
  }
  try {
    const result = await adminLogin(adminUser.value.trim(), adminPassword.value)
    if (!result.success) throw new Error(result.message ?? '登录失败')
    window.location.href = '/admin'
  } catch (error) {
    adminError.value = error instanceof Error ? error.message : '登录失败'
  }
}

async function signOut(): Promise<void> {
  try { await logout() } finally {
    sessionStorage.setItem('ycloud-stay-signed-out', '1')
    window.location.replace('/v2/')
  }
}

onMounted(refresh)
</script>

<template>
  <header class="topbar browser-topbar">
    <div class="brand"><CloudIcon /><span>Ycloud</span></div>
    <label class="top-search">
      <svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="8" /><path d="m21 21-4.35-4.35" /></svg>
      <input v-model="query" type="search" placeholder="搜索当前目录" aria-label="搜索当前目录">
    </label>
    <div class="top-actions">
      <button class="icon-btn flat" type="button" title="管理员" aria-label="管理员" @click="openAdmin"><svg class="ui-icon" viewBox="0 0 24 24"><path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67 0C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.2 1.2 0 0 1 1.52 0C14.5 3.8 17 5 19 5a1 1 0 0 1 1 1z" /><circle cx="12" cy="11" r="3" /></svg></button>
      <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
      <button class="icon-btn flat" type="button" title="退出登录" aria-label="退出登录" @click="signOut"><svg class="ui-icon" viewBox="0 0 24 24"><path d="m16 17 5-5-5-5M21 12H9M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /></svg></button>
    </div>
  </header>

  <main class="browser-page">
    <nav class="breadcrumb" aria-label="当前位置">
      <button class="crumb" :aria-current="crumbs.length ? undefined : 'location'" type="button" @click="navigate('')">/</button>
      <template v-for="crumb in crumbs" :key="crumb.path">
        <span class="crumb-separator" aria-hidden="true">›</span>
        <button class="crumb" :aria-current="crumb.path === path ? 'location' : undefined" type="button" @click="navigate(crumb.path)">{{ crumb.label }}</button>
      </template>
    </nav>

    <section class="file-panel glass" :aria-busy="loading">
      <div class="file-head">
        <button class="select-box" :class="{ checked: allSelected }" type="button" aria-label="全选" @click="toggleSelectAll"><span class="visually-hidden">全选</span></button>
        <button class="sort-btn" type="button" @click="changeSort('name')">名称 <span>{{ sort === 'name' ? (ascending ? '▲' : '▼') : '' }}</span></button>
        <button class="sort-btn right modified" type="button" @click="changeSort('time')">修改时间 <span>{{ sort === 'time' ? (ascending ? '▲' : '▼') : '' }}</span></button>
        <button class="sort-btn right" type="button" @click="changeSort('size')">大小 <span>{{ sort === 'size' ? (ascending ? '▲' : '▼') : '' }}</span></button>
      </div>
      <div v-if="loading" class="empty">正在加载…</div>
      <div v-else-if="!visibleEntries.length" class="empty">{{ query ? '没有匹配的文件' : '此文件夹为空' }}</div>
      <div v-else>
        <div
          v-for="entry in visibleEntries"
          :key="entry.path"
          class="file-row"
          :class="{ selected: selected.has(entry.path) }"
          @click="toggleSelection(entry.path)"
          @dblclick="openEntry(entry)"
        >
          <button class="select-box" :class="{ checked: selected.has(entry.path) }" type="button" :aria-label="`选择 ${entry.name}`" @click.stop="toggleSelection(entry.path)"><span class="visually-hidden">选择 {{ entry.name }}</span></button>
          <div class="file-name"><FileIcon :entry="entry" /><span class="file-label">{{ entry.name }}</span></div>
          <div class="cell right modified">{{ entry.modified || '-' }}</div>
          <div class="cell right">{{ entry.is_dir ? '-' : formatSize(entry.size) }}</div>
        </div>
      </div>
    </section>
    <p v-if="truncated" class="browser-warning">当前目录仅显示服务器允许的部分项目</p>
  </main>

  <div class="toast" :class="{ show: notice }" role="status">{{ notice }}</div>

  <div v-if="showUnlock" class="overlay active" @click.self="showUnlock = false">
    <form class="modal" @submit.prevent="submitUnlock">
      <h2>请输入</h2><p>此文件夹已锁定，请输入密码：</p>
      <input v-model="unlockPassword" class="input" type="password" autocomplete="current-password" autofocus>
      <p class="modal-error">{{ unlockError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="showUnlock = false">取消</button><button class="btn" type="submit">确定</button></div>
    </form>
  </div>

  <div v-if="showAdmin" class="overlay active" @click.self="showAdmin = false">
    <form class="modal" @submit.prevent="submitAdmin">
      <h2>管理员登录</h2>
      <label>用户名<input v-model="adminUser" class="input" autocomplete="username"></label>
      <label>密码<input v-model="adminPassword" class="input" type="password" autocomplete="current-password"></label>
      <p class="modal-error">{{ adminError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="showAdmin = false">取消</button><button class="btn" type="submit">登录</button></div>
    </form>
  </div>
</template>

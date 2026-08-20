<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import type { BatchOperation, BatchResponse, FileEntry } from '../../shared/api/browser'
import { adminLogin, batchOperation, createFolder, downloadUrl, listFiles, logout, prepareArchive, renameItem, unlockFolder, uploadFile } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import BrowserContextMenu from './BrowserContextMenu.vue'
import type { BrowserAction } from './BrowserActionIcon.vue'
import FileIcon from './FileIcon.vue'
import FolderPicker from './FolderPicker.vue'

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
const maxUploadBytes = ref(0)
const maxArchiveBytes = ref(0)
const maxArchiveEntries = ref(0)
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
const fileInput = ref<HTMLInputElement>()
const showFolder = ref(false)
const folderName = ref('')
const folderError = ref('')
const creatingFolder = ref(false)
const showUpload = ref(false)
const uploading = ref(false)
const uploadCurrent = ref('')
const uploadProcessed = ref(0)
const uploadTotal = ref(0)
const uploadSummary = ref('')
const contextVisible = ref(false)
const contextEntry = ref<FileEntry | null>(null)
const contextX = ref(0)
const contextY = ref(0)
const showRename = ref(false)
const renameTarget = ref('')
const renameName = ref('')
const renameError = ref('')
const renaming = ref(false)
const showDelete = ref(false)
const pendingDelete = ref<string[]>([])
const operationBusy = ref(false)
const operationError = ref('')
const pickerOperation = ref<'move' | 'copy' | null>(null)
const pickerPaths = ref<string[]>([])
const batchResult = ref<BatchResponse | null>(null)

const uploadPercent = computed(() => uploadTotal.value ? Math.min(100, Math.round(uploadProcessed.value / uploadTotal.value * 100)) : 0)

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
const selectedPaths = computed(() => [...selected.value])
const pickerTitle = computed(() => `${pickerOperation.value === 'move' ? '移动' : '复制'} ${pickerPaths.value.length} 个项目到…`)

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
    maxUploadBytes.value = data.max_upload_bytes
    maxArchiveBytes.value = data.max_archive_bytes
    maxArchiveEntries.value = data.max_archive_entries
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
  syncMobileMenu(next)
}

function toggleSelectAll(): void {
  const next = new Set(selected.value)
  if (allSelected.value) visibleEntries.value.forEach(entry => next.delete(entry.path))
  else visibleEntries.value.forEach(entry => next.add(entry.path))
  selected.value = next
  syncMobileMenu(next)
}

function isMobileLayout(): boolean {
  return window.matchMedia('(max-width: 760px)').matches
}

function syncMobileMenu(paths: Set<string>): void {
  if (!isMobileLayout()) return
  if (!paths.size) {
    contextVisible.value = false
    return
  }
  contextEntry.value = paths.size === 1 ? entries.value.find(entry => paths.has(entry.path)) ?? null : null
  contextX.value = 0
  contextY.value = 0
  contextVisible.value = true
}

function openRowMenu(event: MouseEvent, entry: FileEntry): void {
  event.preventDefault()
  if (!selected.value.has(entry.path)) selected.value = new Set([entry.path])
  contextEntry.value = entry
  contextX.value = event.clientX
  contextY.value = event.clientY
  contextVisible.value = true
}

function openBackgroundMenu(event: MouseEvent): void {
  if ((event.target as HTMLElement).closest('.file-row')) return
  if (!canWrite.value) return
  event.preventDefault()
  selected.value = new Set()
  contextEntry.value = null
  contextX.value = event.clientX
  contextY.value = event.clientY
  contextVisible.value = true
}

function closeContextMenu(): void {
  contextVisible.value = false
}

function clearSelection(): void {
  selected.value = new Set()
  closeContextMenu()
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

function requireWrite(): boolean {
  if (canWrite.value) return true
  void openAdmin()
  return false
}

function openFolderDialog(): void {
  if (!requireWrite()) return
  folderName.value = ''
  folderError.value = ''
  showFolder.value = true
}

async function submitFolder(): Promise<void> {
  const name = folderName.value.trim()
  if (!name || creatingFolder.value) return
  creatingFolder.value = true
  folderError.value = ''
  try {
    await createFolder(path.value, name)
    showFolder.value = false
    announce('文件夹已创建')
    await refresh()
  } catch (error) {
    folderError.value = error instanceof Error ? error.message : '创建失败'
  } finally {
    creatingFolder.value = false
  }
}

function chooseFiles(): void {
  if (requireWrite()) fileInput.value?.click()
}

async function uploadFiles(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement
  const files = Array.from(input.files ?? [])
  input.value = ''
  if (!files.length || !canWrite.value || uploading.value) return

  const accepted = files.filter(file => !maxUploadBytes.value || file.size <= maxUploadBytes.value)
  const rejected = files.length - accepted.length
  uploadTotal.value = accepted.reduce((sum, file) => sum + file.size, 0)
  uploadProcessed.value = 0
  uploadCurrent.value = '正在准备…'
  uploadSummary.value = `共 ${files.length} 个文件，逐个安全上传`
  showUpload.value = true
  uploading.value = true
  let completedBytes = 0
  let succeeded = 0
  let failed = rejected

  try {
    for (const file of accepted) {
      uploadCurrent.value = file.name
      let currentLoaded = 0
      try {
        const target = [path.value, file.name].filter(Boolean).join('/')
        await uploadFile(target, file, loaded => {
          currentLoaded = loaded
          uploadProcessed.value = completedBytes + loaded
        })
        succeeded += 1
      } catch (error) {
        failed += 1
        announce(`${file.name}：${error instanceof Error ? error.message : '上传失败'}`)
      }
      completedBytes += currentLoaded
      uploadProcessed.value = completedBytes
    }
    uploadSummary.value = failed ? `上传结束：成功 ${succeeded} 个，失败 ${failed} 个` : `上传完成：成功 ${succeeded} 个文件`
    await refresh()
  } finally {
    uploading.value = false
  }
}

function entryForPath(entryPath: string): FileEntry | undefined {
  return entries.value.find(entry => entry.path === entryPath)
}

function startDownload(entryPath: string): void {
  window.location.href = downloadUrl(entryPath)
}

async function startArchive(paths: string[]): Promise<void> {
  if (!paths.length) return
  try {
    const result = await prepareArchive(paths)
    announce(`正在打包 ${result.file_count} 个文件、${result.entry_count} 个条目（${formatSize(result.total_bytes)}）`)
    window.location.href = `/api/archive?ticket=${encodeURIComponent(result.ticket)}`
  } catch (error) {
    const message = error instanceof Error ? error.message : '打包准备失败'
    if (/payload too large|too large|大小/i.test(message)) announce(`所选文件总大小超过 ${formatSize(maxArchiveBytes.value)}，请拆分选择`)
    else if (/entry limit|条目/i.test(message)) announce(`打包最多包含 ${maxArchiveEntries.value} 个条目，请拆分选择`)
    else announce(message)
  }
}

function openRenameDialog(entryPath: string): void {
  if (!requireWrite()) return
  renameTarget.value = entryPath
  renameName.value = entryForPath(entryPath)?.name ?? entryPath.split('/').pop() ?? ''
  renameError.value = ''
  showRename.value = true
}

async function submitRename(): Promise<void> {
  const name = renameName.value.trim()
  if (!name || renaming.value) return
  renaming.value = true
  renameError.value = ''
  try {
    await renameItem(path.value, renameTarget.value, name)
    showRename.value = false
    announce('重命名成功')
    await refresh()
  } catch (error) {
    renameError.value = error instanceof Error ? error.message : '重命名失败'
  } finally {
    renaming.value = false
  }
}

function requestTransfer(operation: 'move' | 'copy', paths: string[]): void {
  if (!paths.length || !requireWrite()) return
  pickerOperation.value = operation
  pickerPaths.value = [...paths]
  operationError.value = ''
}

function requestDelete(paths: string[]): void {
  if (!paths.length || !requireWrite()) return
  pendingDelete.value = [...paths]
  operationError.value = ''
  showDelete.value = true
}

async function runBatch(operation: BatchOperation, paths: string[], target = ''): Promise<void> {
  operationBusy.value = true
  operationError.value = ''
  try {
    const result = await batchOperation(operation, paths, target)
    const label = operation === 'move' ? '移动' : operation === 'copy' ? '复制' : '删除'
    if (result.failed) batchResult.value = result
    announce(result.failed ? `${label}成功 ${result.success} 项，失败 ${result.failed} 项` : `${label}成功`)
    selected.value = new Set()
    await refresh()
  } catch (error) {
    operationError.value = error instanceof Error ? error.message : '操作失败'
    announce(operationError.value)
  } finally {
    operationBusy.value = false
  }
}

async function confirmTransfer(target: string): Promise<void> {
  const operation = pickerOperation.value
  const paths = [...pickerPaths.value]
  pickerOperation.value = null
  if (operation) await runBatch(operation, paths, target)
}

async function confirmDelete(): Promise<void> {
  const paths = [...pendingDelete.value]
  await runBatch('delete', paths)
  if (!operationError.value) showDelete.value = false
}

function handleMenuAction(action: BrowserAction): void {
  const paths = [...selected.value]
  const entry = contextEntry.value ?? (paths.length === 1 ? entryForPath(paths[0] ?? '') ?? null : null)
  closeContextMenu()
  switch (action) {
    case 'open':
      if (entry) openEntry(entry)
      break
    case 'download':
      if (paths[0]) startDownload(paths[0])
      break
    case 'archive':
      void startArchive(paths)
      break
    case 'rename':
      if (paths[0]) openRenameDialog(paths[0])
      break
    case 'move':
    case 'copy':
      requestTransfer(action, paths)
      break
    case 'delete':
      requestDelete(paths)
      break
    case 'upload':
      chooseFiles()
      break
    case 'mkdir':
      openFolderDialog()
      break
    case 'folder':
      break
  }
}

async function signOut(): Promise<void> {
  try { await logout() } finally {
    sessionStorage.setItem('ycloud-stay-signed-out', '1')
    window.location.replace('/v2/')
  }
}

function handleEscape(event: KeyboardEvent): void {
  if (event.key === 'Escape') closeContextMenu()
}

function handleDocumentClick(): void {
  if (isMobileLayout() && selected.value.size) return
  closeContextMenu()
}

onMounted(() => {
  void refresh()
  document.addEventListener('click', handleDocumentClick)
  document.addEventListener('keydown', handleEscape)
})
onBeforeUnmount(() => {
  document.removeEventListener('click', handleDocumentClick)
  document.removeEventListener('keydown', handleEscape)
})
</script>

<template>
  <header class="topbar browser-topbar">
    <div class="brand"><CloudIcon /><span>Ycloud</span></div>
    <label class="top-search">
      <svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="8" /><path d="m21 21-4.35-4.35" /></svg>
      <input v-model="query" type="search" placeholder="搜索当前目录" aria-label="搜索当前目录">
    </label>
    <div class="top-actions">
      <button class="icon-btn flat" type="button" title="管理员" aria-label="管理员" @click="openAdmin"><svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67 0C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.2 1.2 0 0 1 1.52 0C14.5 3.8 17 5 19 5a1 1 0 0 1 1 1z" /><circle cx="12" cy="11" r="3" /></svg></button>
      <button class="icon-btn flat" type="button" title="新建文件夹" aria-label="新建文件夹" @click="openFolderDialog"><svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /><path d="M12 11v6M9 14h6" /></svg></button>
      <button class="icon-btn flat" type="button" title="上传文件" aria-label="上传文件" @click="chooseFiles"><svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 16V4M7 9l5-5 5 5" /><path d="M20 15v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-4" /></svg></button>
      <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
      <button class="icon-btn flat" type="button" title="退出登录" aria-label="退出登录" @click="signOut"><svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="m16 17 5-5-5-5M21 12H9M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /></svg></button>
    </div>
    <input ref="fileInput" class="visually-hidden" type="file" multiple @change="uploadFiles">
  </header>

  <main class="browser-page" :class="{ 'selection-active': selected.size > 0 }">
    <nav class="breadcrumb" aria-label="当前位置">
      <button class="crumb" :aria-current="crumbs.length ? undefined : 'location'" type="button" @click="navigate('')">/</button>
      <template v-for="crumb in crumbs" :key="crumb.path">
        <span class="crumb-separator" aria-hidden="true">›</span>
        <button class="crumb" :aria-current="crumb.path === path ? 'location' : undefined" type="button" @click="navigate(crumb.path)">{{ crumb.label }}</button>
      </template>
    </nav>

    <section class="file-panel glass" :aria-busy="loading" @contextmenu="openBackgroundMenu">
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
          @contextmenu.stop="openRowMenu($event, entry)"
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

  <BrowserContextMenu
    v-if="contextVisible"
    :entry="contextEntry"
    :paths="selectedPaths"
    :can-write="canWrite"
    :x="contextX"
    :y="contextY"
    @action="handleMenuAction"
    @clear="clearSelection"
  />

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

  <div v-if="showFolder" class="overlay active" @click.self="showFolder = false">
    <form class="modal" @submit.prevent="submitFolder">
      <h2>新建文件夹</h2>
      <label>名称<input v-model="folderName" class="input" autocomplete="off" autofocus></label>
      <p class="modal-error">{{ folderError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="creatingFolder" @click="showFolder = false">取消</button><button class="btn" type="submit" :disabled="creatingFolder">{{ creatingFolder ? '创建中…' : '创建' }}</button></div>
    </form>
  </div>

  <div v-if="showUpload" class="overlay active" @click.self="!uploading && (showUpload = false)">
    <section class="modal upload-modal" aria-labelledby="upload-title">
      <h2 id="upload-title">上传文件</h2>
      <p class="upload-current">{{ uploadCurrent }}</p>
      <progress :value="uploadPercent" max="100">{{ uploadPercent }}%</progress>
      <p>{{ uploadPercent }}% · {{ formatSize(uploadProcessed) }} / {{ formatSize(uploadTotal) }}</p>
      <p class="upload-summary">{{ uploadSummary }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="uploading" @click="showUpload = false">关闭</button></div>
    </section>
  </div>

  <div v-if="showRename" class="overlay active" @click.self="showRename = false">
    <form class="modal" @submit.prevent="submitRename">
      <h2>重命名</h2>
      <label>新名称<input v-model="renameName" class="input" autocomplete="off" autofocus></label>
      <p class="modal-error">{{ renameError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="renaming" @click="showRename = false">取消</button><button class="btn" type="submit" :disabled="renaming">{{ renaming ? '保存中…' : '保存' }}</button></div>
    </form>
  </div>

  <FolderPicker v-if="pickerOperation" :title="pickerTitle" @close="pickerOperation = null" @confirm="confirmTransfer" />

  <div v-if="showDelete" class="overlay active" @click.self="!operationBusy && (showDelete = false)">
    <section class="modal" aria-labelledby="delete-title">
      <h2 id="delete-title">确认永久删除</h2>
      <p>将永久删除 {{ pendingDelete.length }} 个项目，此操作无法撤销。</p>
      <p class="modal-error">{{ operationError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="operationBusy" @click="showDelete = false">取消</button><button class="btn danger" type="button" :disabled="operationBusy" @click="confirmDelete">{{ operationBusy ? '删除中…' : `删除 (${pendingDelete.length})` }}</button></div>
    </section>
  </div>

  <div v-if="batchResult" class="overlay active" @click.self="batchResult = null">
    <section class="modal result-modal" aria-labelledby="result-title">
      <h2 id="result-title">部分项目未完成</h2>
      <p>成功 {{ batchResult.success }} 项，失败 {{ batchResult.failed }} 项。</p>
      <div class="result-list">
        <div v-for="item in batchResult.results.filter(result => result.status >= 400)" :key="item.path" class="result-row">
          <strong>{{ item.path }}</strong><span>{{ item.message }}（{{ item.code }}）</span>
        </div>
      </div>
      <div class="modal-actions"><button class="btn" type="button" @click="batchResult = null">确定</button></div>
    </section>
  </div>
</template>

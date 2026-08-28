<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import type { BatchOperation, BatchResponse, BrowserCapabilities, BrowserStorage, FileEntry } from '../../shared/api/browser'
import { adminLogin, batchOperation, createFolder, downloadUrl, listFiles, listStorages, logout, prepareArchive, renameItem, unlockFolder, uploadFile } from '../../shared/api/browser'
import { appPath } from '../../shared/routes'
import { formatSize } from '../../shared/format'
import AppIcon from '../../shared/components/AppIcon.vue'
import AppSelect from '../../shared/components/AppSelect.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import BrowserContextMenu from './BrowserContextMenu.vue'
import type { BrowserAction } from './BrowserActionIcon.vue'
import FileIcon from './FileIcon.vue'
import FolderPicker from './FolderPicker.vue'

type SortKey = 'name' | 'time' | 'size'
type PageSize = 10 | 20 | 50 | 100
type DragSession = { startX: number; startY: number; active: boolean; initialSelection: Set<string> }

const DRAG_THRESHOLD = 6
const PAGE_SIZES: PageSize[] = [10, 20, 50, 100]

defineProps<{ theme: ThemeController }>()
const locale = useLocale()
const path = ref('')
const currentStorageId = ref('')
const storages = ref<BrowserStorage[]>([])
const entries = ref<FileEntry[]>([])
const query = ref('')
const appliedQuery = ref('')
const sort = ref<SortKey>('name')
const ascending = ref(true)
const pageSize = ref<PageSize>(20)
const currentCursor = ref<string>()
const nextCursor = ref<string | null>(null)
const cursorHistory = ref<Array<string | undefined>>([])
const selected = ref(new Set<string>())
const loading = ref(true)
const canWrite = ref(false)
const isAdministrator = ref(false)
const capabilities = ref<BrowserCapabilities>({ download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false })
const maxUploadBytes = ref(0)
const maxArchiveBytes = ref(0)
const maxArchiveEntries = ref(0)
const notice = ref('')
const showUnlock = ref(false)
const unlockPath = ref('')
const unlockPassword = ref('')
const unlockError = ref('')
const showAdmin = ref(false)
const adminUser = ref('')
const adminPassword = ref('')
const adminError = ref('')
const pendingStorageId = ref('')
const fileInput = ref<HTMLInputElement>()
const filePanel = ref<HTMLElement>()
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
const dragSelecting = ref(false)
let dragSession: DragSession | null = null
let suppressRowClick = false
let suppressRowClickTimer: number | undefined
let searchTimer: number | undefined
let refreshSequence = 0

const uploadPercent = computed(() => uploadTotal.value ? Math.min(100, Math.round(uploadProcessed.value / uploadTotal.value * 100)) : 0)

const visibleEntries = computed(() => entries.value)
const pageNumber = computed(() => cursorHistory.value.length + 1)
const storageOptions = computed(() => storages.value.map(storage => ({
  value: storage.id,
  label: `${storage.name}${storage.requires_login ? locale.text('（需登录）', ' (sign in)') : ''}`,
})))
const pageSizeOptions = PAGE_SIZES.map(size => ({ value: size, label: String(size) }))

const crumbs = computed(() => {
  let accumulated = ''
  return path.value.split('/').filter(Boolean).map(label => {
    accumulated = accumulated ? `${accumulated}/${label}` : label
    return { label, path: accumulated }
  })
})

const allSelected = computed(() => visibleEntries.value.length > 0 && visibleEntries.value.every(entry => selected.value.has(entry.path)))
const selectedPaths = computed(() => [...selected.value])
const pickerTitle = computed(() => locale.text(
  `${pickerOperation.value === 'move' ? '移动' : '复制'} ${pickerPaths.value.length} 个项目到…`,
  `${pickerOperation.value === 'move' ? 'Move' : 'Copy'} ${pickerPaths.value.length} item(s) to…`,
))
function announce(message: string): void {
  notice.value = message
  window.setTimeout(() => { if (notice.value === message) notice.value = '' }, 2800)
}

async function refresh(): Promise<void> {
  const sequence = ++refreshSequence
  loading.value = true
  try {
    const data = await listFiles(path.value, currentStorageId.value || undefined, {
      limit: pageSize.value,
      cursor: currentCursor.value,
      search: appliedQuery.value || undefined,
      sort: sort.value,
      direction: ascending.value ? 'asc' : 'desc',
    })
    if (sequence !== refreshSequence) return
    currentStorageId.value = data.storage_id
    storages.value = data.storages ?? []
    path.value = data.current_path.replace(/^\/+|\/+$/g, '')
    entries.value = data.entries
    nextCursor.value = data.next_cursor ?? null
    canWrite.value = data.can_write
    isAdministrator.value = Boolean(data.is_admin)
    capabilities.value = data.capabilities ?? {
      download: true,
      upload: data.can_write,
      create_directory: data.can_write,
      rename: data.can_write,
      move_items: data.can_write,
      copy: data.can_write,
      delete: data.can_write,
    }
    maxUploadBytes.value = data.max_upload_bytes
    maxArchiveBytes.value = data.max_archive_bytes
    maxArchiveEntries.value = data.max_archive_entries
    selected.value = new Set()
  } catch (error) {
    if (sequence !== refreshSequence) return
    if (!storages.value.length) {
      try { storages.value = await listStorages() } catch { /* Keep the original file-list error. */ }
    }
    announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
  } finally {
    if (sequence === refreshSequence) loading.value = false
  }
}

function resetPagination(): void {
  currentCursor.value = undefined
  nextCursor.value = null
  cursorHistory.value = []
  selected.value = new Set()
}

function scheduleSearch(): void {
  if (searchTimer !== undefined) window.clearTimeout(searchTimer)
  searchTimer = window.setTimeout(() => {
    appliedQuery.value = query.value.trim()
    resetPagination()
    void refresh()
  }, 250)
}

async function navigate(destination: string): Promise<void> {
  path.value = destination.replace(/^\/+|\/+$/g, '')
  query.value = ''
  appliedQuery.value = ''
  if (searchTimer !== undefined) window.clearTimeout(searchTimer)
  resetPagination()
  await refresh()
}

async function switchStorage(value: string | number): Promise<void> {
  const nextStorageId = String(value)
  const nextStorage = storages.value.find(storage => storage.id === nextStorageId)
  if (!nextStorage || nextStorage.requires_login) {
    pendingStorageId.value = nextStorageId
    void openAdmin(true)
    return
  }
  currentStorageId.value = nextStorageId
  path.value = ''
  query.value = ''
  appliedQuery.value = ''
  if (searchTimer !== undefined) window.clearTimeout(searchTimer)
  resetPagination()
  await refresh()
}

function changeSort(key: SortKey): void {
  if (sort.value === key) ascending.value = !ascending.value
  else {
    sort.value = key
    ascending.value = true
  }
  resetPagination()
  void refresh()
}

function changePageSize(): void {
  resetPagination()
  void refresh()
}

function nextPage(): void {
  if (!nextCursor.value || loading.value) return
  cursorHistory.value = [...cursorHistory.value, currentCursor.value]
  currentCursor.value = nextCursor.value
  selected.value = new Set()
  void refresh()
}

function previousPage(): void {
  if (!cursorHistory.value.length || loading.value) return
  const history = [...cursorHistory.value]
  currentCursor.value = history.pop()
  cursorHistory.value = history
  selected.value = new Set()
  void refresh()
}

function toggleSelection(entryPath: string): void {
  const next = new Set(selected.value)
  if (next.has(entryPath)) next.delete(entryPath)
  else next.add(entryPath)
  selected.value = next
  syncMobileMenu(next)
}

function handleRowClick(entryPath: string): void {
  if (suppressRowClick) {
    suppressRowClick = false
    if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
    return
  }
  toggleSelection(entryPath)
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

function startDragSelection(event: MouseEvent): void {
  if (event.button !== 0 || isMobileLayout() || loading.value || !visibleEntries.value.length) return
  const target = event.target as HTMLElement
  const button = target.closest('button')
  if (target.closest('input, select, textarea, a') || (button && !button.classList.contains('select-box'))) return
  // Prevent the browser's native text/image drag before the movement threshold.
  // Row checkboxes remain valid drag starting points; an ordinary click still
  // reaches their click handler when no drag actually occurs.
  event.preventDefault()
  dragSession = {
    startX: event.clientX,
    startY: event.clientY,
    active: false,
    initialSelection: new Set(selected.value),
  }
  closeContextMenu()
}

function updateDragSelection(event: MouseEvent): void {
  const session = dragSession
  const panel = filePanel.value
  if (!session || !panel) return
  if ((event.buttons & 1) === 0) {
    cancelDragSelection()
    return
  }
  if (!session.active && Math.hypot(event.clientX - session.startX, event.clientY - session.startY) < DRAG_THRESHOLD) return

  session.active = true
  dragSelecting.value = true
  event.preventDefault()
  window.getSelection()?.removeAllRanges()
  const panelRect = panel.getBoundingClientRect()
  const currentX = Math.min(panelRect.right, Math.max(panelRect.left, event.clientX))
  const currentY = Math.min(panelRect.bottom, Math.max(panelRect.top, event.clientY))
  const left = Math.min(session.startX, currentX)
  const top = Math.min(session.startY, currentY)
  const right = Math.max(session.startX, currentX)
  const bottom = Math.max(session.startY, currentY)
  const next = new Set(session.initialSelection)
  const selecting = currentY >= session.startY
  panel.querySelectorAll<HTMLElement>('.file-row[data-entry-path]').forEach(row => {
    const rowRect = row.getBoundingClientRect()
    const intersects = left < rowRect.right && right > rowRect.left && top < rowRect.bottom && bottom > rowRect.top
    const entryPath = row.dataset.entryPath
    if (!intersects || !entryPath) return
    if (selecting) next.add(entryPath)
    else next.delete(entryPath)
  })
  selected.value = next
}

function finishDragSelection(): void {
  if (!dragSession) return
  const wasActive = dragSession.active
  dragSession = null
  dragSelecting.value = false
  if (!wasActive) return
  suppressRowClick = true
  suppressRowClickTimer = window.setTimeout(() => { suppressRowClick = false }, 0)
}

function cancelDragSelection(): void {
  dragSession = null
  dragSelecting.value = false
}

function blockExternalFileDrop(event: DragEvent): void {
  if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
  event.preventDefault()
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'none'
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
  if (!capabilities.value.upload && !capabilities.value.create_directory) return
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
  if (!capabilities.value.download) return
  const params = new URLSearchParams({ path: `/${entry.path}`, storage_id: currentStorageId.value })
  window.open(`${appPath('/preview')}?${params.toString()}`, '_blank', 'noopener')
}

async function submitUnlock(): Promise<void> {
  if (!unlockPassword.value) return
  try {
    const result = await unlockFolder(unlockPath.value, unlockPassword.value, currentStorageId.value)
    if (!result.success) throw new Error(result.message ?? locale.text('密码错误', 'Incorrect password'))
    showUnlock.value = false
    await navigate(unlockPath.value)
  } catch (error) {
    unlockError.value = error instanceof Error ? error.message : locale.text('解锁失败', 'Unable to unlock this folder')
  }
}

async function openAdmin(forStorage = false): Promise<void> {
  if (isAdministrator.value && !forStorage) {
    window.location.href = appPath('/admin/account')
    return
  }
  adminUser.value = ''
  adminPassword.value = ''
  adminError.value = ''
  showAdmin.value = true
}

async function submitAdmin(): Promise<void> {
  if (!adminUser.value.trim() || !adminPassword.value) {
    adminError.value = locale.text('请输入用户名和密码', 'Enter your username and password')
    return
  }
  try {
    const result = await adminLogin(adminUser.value.trim(), adminPassword.value)
    if (!result.success) throw new Error(result.message ?? locale.text('登录失败', 'Sign-in failed'))
    showAdmin.value = false
    adminPassword.value = ''
    isAdministrator.value = result.is_admin
    if (pendingStorageId.value) {
      currentStorageId.value = pendingStorageId.value
      pendingStorageId.value = ''
      path.value = ''
      resetPagination()
      await refresh()
    } else if (result.is_admin) window.location.href = appPath('/admin/account')
    else await refresh()
  } catch (error) {
    adminError.value = error instanceof Error ? error.message : locale.text('登录失败', 'Sign-in failed')
  }
}

function requireCapability(action: keyof BrowserCapabilities): boolean {
  if (capabilities.value[action]) return true
  void openAdmin()
  return false
}

function openFolderDialog(): void {
  if (!requireCapability('create_directory')) return
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
    await createFolder(path.value, name, currentStorageId.value)
    showFolder.value = false
    announce(locale.text('文件夹已创建', 'Folder created'))
    await refresh()
  } catch (error) {
    folderError.value = error instanceof Error ? error.message : locale.text('创建失败', 'Unable to create the folder')
  } finally {
    creatingFolder.value = false
  }
}

function chooseFiles(): void {
  if (requireCapability('upload')) fileInput.value?.click()
}

async function uploadFiles(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement
  const files = Array.from(input.files ?? [])
  input.value = ''
  if (!files.length || !capabilities.value.upload || uploading.value) return

  const accepted = files.filter(file => !maxUploadBytes.value || file.size <= maxUploadBytes.value)
  const rejected = files.length - accepted.length
  uploadTotal.value = accepted.reduce((sum, file) => sum + file.size, 0)
  uploadProcessed.value = 0
  uploadCurrent.value = locale.text('正在准备…', 'Preparing…')
  uploadSummary.value = locale.text(`共 ${files.length} 个文件，逐个安全上传`, `Uploading ${files.length} file(s) sequentially`)
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
        }, currentStorageId.value)
        succeeded += 1
      } catch (error) {
        failed += 1
        announce(`${file.name}: ${error instanceof Error ? error.message : locale.text('上传失败', 'Upload failed')}`)
      }
      completedBytes += currentLoaded
      uploadProcessed.value = completedBytes
    }
    uploadSummary.value = failed
      ? locale.text(`上传结束：成功 ${succeeded} 个，失败 ${failed} 个`, `Upload finished: ${succeeded} succeeded, ${failed} failed`)
      : locale.text(`上传完成：成功 ${succeeded} 个文件`, `Upload complete: ${succeeded} file(s)`)
    await refresh()
  } finally {
    uploading.value = false
  }
}

function entryForPath(entryPath: string): FileEntry | undefined {
  return entries.value.find(entry => entry.path === entryPath)
}

function startDownload(entryPath: string): void {
  if (!capabilities.value.download) return
  window.location.href = downloadUrl(entryPath, currentStorageId.value)
}

async function startArchive(paths: string[]): Promise<void> {
  if (!paths.length || !capabilities.value.download) return
  try {
    const result = await prepareArchive(paths, currentStorageId.value)
    announce(locale.text(
      `正在打包 ${result.file_count} 个文件、${result.entry_count} 个条目（${formatSize(result.total_bytes)}）`,
      `Preparing ${result.file_count} file(s), ${result.entry_count} entries (${formatSize(result.total_bytes)})`,
    ))
    window.location.href = `/api/archive?ticket=${encodeURIComponent(result.ticket)}`
  } catch (error) {
    const message = error instanceof Error ? error.message : locale.text('打包准备失败', 'Unable to prepare the archive')
    if (/payload too large|too large|大小/i.test(message)) announce(locale.text(`所选文件总大小超过 ${formatSize(maxArchiveBytes.value)}，请拆分选择`, `The selection exceeds ${formatSize(maxArchiveBytes.value)}; split it into smaller groups`))
    else if (/entry limit|条目/i.test(message)) announce(locale.text(`打包最多包含 ${maxArchiveEntries.value} 个条目，请拆分选择`, `An archive can contain at most ${maxArchiveEntries.value} entries; split the selection`))
    else announce(message)
  }
}

function openRenameDialog(entryPath: string): void {
  if (!requireCapability('rename')) return
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
    await renameItem(path.value, renameTarget.value, name, currentStorageId.value)
    showRename.value = false
    announce(locale.text('重命名成功', 'Renamed'))
    await refresh()
  } catch (error) {
    renameError.value = error instanceof Error ? error.message : locale.text('重命名失败', 'Unable to rename the item')
  } finally {
    renaming.value = false
  }
}

function requestTransfer(operation: 'move' | 'copy', paths: string[]): void {
  if (!paths.length || !requireCapability(operation === 'move' ? 'move_items' : 'copy')) return
  pickerOperation.value = operation
  pickerPaths.value = [...paths]
  operationError.value = ''
}

function requestDelete(paths: string[]): void {
  if (!paths.length || !requireCapability('delete')) return
  pendingDelete.value = [...paths]
  operationError.value = ''
  showDelete.value = true
}

async function runBatch(operation: BatchOperation, paths: string[], target = ''): Promise<void> {
  operationBusy.value = true
  operationError.value = ''
  try {
    const result = await batchOperation(operation, paths, target, currentStorageId.value)
    const label = operation === 'move'
      ? locale.text('移动', 'Move')
      : operation === 'copy' ? locale.text('复制', 'Copy') : locale.text('删除', 'Delete')
    if (result.failed) batchResult.value = result
    announce(result.failed
      ? locale.text(`${label}成功 ${result.success} 项，失败 ${result.failed} 项`, `${label}: ${result.success} succeeded, ${result.failed} failed`)
      : locale.text(`${label}成功`, `${label} completed`))
    selected.value = new Set()
    await refresh()
  } catch (error) {
    operationError.value = error instanceof Error ? error.message : locale.t('common.failed')
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
    window.location.replace(appPath('/'))
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
  window.addEventListener('mousemove', updateDragSelection, { passive: false })
  window.addEventListener('mouseup', finishDragSelection)
  window.addEventListener('blur', cancelDragSelection)
  window.addEventListener('dragover', blockExternalFileDrop)
  window.addEventListener('drop', blockExternalFileDrop)
})
onBeforeUnmount(() => {
  document.removeEventListener('click', handleDocumentClick)
  document.removeEventListener('keydown', handleEscape)
  window.removeEventListener('mousemove', updateDragSelection)
  window.removeEventListener('mouseup', finishDragSelection)
  window.removeEventListener('blur', cancelDragSelection)
  window.removeEventListener('dragover', blockExternalFileDrop)
  window.removeEventListener('drop', blockExternalFileDrop)
  if (suppressRowClickTimer !== undefined) window.clearTimeout(suppressRowClickTimer)
  if (searchTimer !== undefined) window.clearTimeout(searchTimer)
})
</script>

<template>
  <header class="browser-chrome glass">
    <div class="brand"><AppIcon name="cloud" :size="28" /><span>Ycloud</span></div>
    <div v-if="storages.length" class="storage-switcher browser-storage-switcher">
      <AppSelect :model-value="currentStorageId" :options="storageOptions" :label="locale.text('切换存储', 'Switch storage')" @change="switchStorage" />
    </div>
    <div class="top-actions">
      <button class="icon-btn flat" type="button" :title="locale.text('账号登录 / 管理员', 'Account sign-in / Administrator')" :aria-label="locale.text('账号登录 / 管理员', 'Account sign-in / Administrator')" @click="openAdmin()"><AppIcon name="administrator" /></button>
      <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
      <LocaleToggle class="flat" />
      <button class="icon-btn flat" type="button" :title="locale.text('退出登录', 'Sign out')" :aria-label="locale.text('退出登录', 'Sign out')" @click="signOut"><AppIcon name="sign-out" /></button>
    </div>
    <input ref="fileInput" class="visually-hidden" type="file" multiple @change="uploadFiles">
  </header>

  <main class="browser-page" :class="{ 'selection-active': selected.size > 0 }">
    <section ref="filePanel" class="file-panel glass" :class="{ 'drag-selecting': dragSelecting }" :aria-busy="loading" @mousedown="startDragSelection" @contextmenu="openBackgroundMenu">
      <div class="file-toolbar">
        <label class="top-search file-search">
          <AppIcon name="search" />
          <input v-model="query" type="search" :placeholder="locale.text('搜索当前目录', 'Search this folder')" :aria-label="locale.text('搜索当前目录', 'Search this folder')" @input="scheduleSearch">
        </label>
        <div class="file-toolbar-actions">
          <button v-if="capabilities.create_directory" class="btn secondary" type="button" @click="openFolderDialog"><AppIcon name="folder-plus" /><span>{{ locale.text('新建文件夹', 'New folder') }}</span></button>
          <button v-if="capabilities.upload" class="btn" type="button" @click="chooseFiles"><AppIcon name="upload" /><span>{{ locale.text('上传文件', 'Upload files') }}</span></button>
        </div>
      </div>

      <nav class="breadcrumb" :aria-label="locale.text('当前位置', 'Current path')">
        <button class="crumb home-crumb" :aria-current="crumbs.length ? undefined : 'location'" :aria-label="locale.text('首页', 'Home')" :title="locale.text('首页', 'Home')" type="button" @click="navigate('')"><AppIcon name="home" /></button>
        <template v-for="crumb in crumbs" :key="crumb.path">
          <span class="crumb-separator" aria-hidden="true">›</span>
          <button class="crumb" :aria-current="crumb.path === path ? 'location' : undefined" type="button" @click="navigate(crumb.path)">{{ crumb.label }}</button>
        </template>
      </nav>

      <div class="file-head">
        <button class="select-box" :class="{ checked: allSelected }" type="button" :aria-label="locale.text('全选', 'Select all')" @click="toggleSelectAll"><span class="visually-hidden">{{ locale.text('全选', 'Select all') }}</span></button>
        <button class="sort-btn" type="button" @click="changeSort('name')">{{ locale.text('名称', 'Name') }} <span>{{ sort === 'name' ? (ascending ? '▲' : '▼') : '' }}</span></button>
        <button class="sort-btn right" type="button" @click="changeSort('size')">{{ locale.text('大小', 'Size') }} <span>{{ sort === 'size' ? (ascending ? '▲' : '▼') : '' }}</span></button>
        <button class="sort-btn right modified" type="button" @click="changeSort('time')">{{ locale.text('修改时间', 'Modified') }} <span>{{ sort === 'time' ? (ascending ? '▲' : '▼') : '' }}</span></button>
      </div>
      <div v-if="loading" class="empty">{{ locale.t('common.loading') }}</div>
      <div v-else-if="!visibleEntries.length" class="empty">{{ appliedQuery ? locale.text('没有匹配的文件', 'No matching files') : locale.text('此文件夹为空', 'This folder is empty') }}</div>
      <div v-else>
        <div
          v-for="entry in visibleEntries"
          :key="entry.path"
          class="file-row"
          :class="{ selected: selected.has(entry.path) }"
          :data-entry-path="entry.path"
          @click="handleRowClick(entry.path)"
          @dblclick="openEntry(entry)"
          @contextmenu.stop="openRowMenu($event, entry)"
        >
          <button class="select-box" :class="{ checked: selected.has(entry.path) }" type="button" :aria-label="locale.text(`选择 ${entry.name}`, `Select ${entry.name}`)" @click.stop="toggleSelection(entry.path)"><span class="visually-hidden">{{ locale.text('选择', 'Select') }} {{ entry.name }}</span></button>
          <div class="file-name"><FileIcon :entry="entry" /><span class="file-label">{{ entry.name }}</span><span v-if="entry.locked" class="file-lock-indicator" :title="locale.t('file.lockedFolder')"><AppIcon name="lock" :size="12" /></span></div>
          <div class="cell right">{{ entry.is_dir ? '-' : formatSize(entry.size) }}</div>
          <div class="cell right modified">{{ entry.modified || '-' }}</div>
        </div>
      </div>
      <footer class="file-pagination">
        <div class="pagination-summary">
          <AppSelect v-model="pageSize" class="page-size-select" placement="top" :options="pageSizeOptions" :label="locale.text('每页显示数量', 'Items per page')" @change="changePageSize" />
        </div>
        <nav class="pagination-controls" :aria-label="locale.text('文件翻页', 'File pagination')">
          <button class="page-arrow" type="button" :disabled="!cursorHistory.length || loading" :title="locale.text('上一页', 'Previous page')" :aria-label="locale.text('上一页', 'Previous page')" @click="previousPage"><span class="page-chevron previous" aria-hidden="true" /></button>
          <span class="current-page" :aria-label="locale.text(`第 ${pageNumber} 页`, `Page ${pageNumber}`)">{{ pageNumber }}</span>
          <button class="page-arrow" type="button" :disabled="!nextCursor || loading" :title="locale.text('下一页', 'Next page')" :aria-label="locale.text('下一页', 'Next page')" @click="nextPage"><span class="page-chevron next" aria-hidden="true" /></button>
        </nav>
      </footer>
    </section>
  </main>

  <BrowserContextMenu
    v-if="contextVisible"
    :entry="contextEntry"
    :paths="selectedPaths"
    :capabilities="capabilities"
    :x="contextX"
    :y="contextY"
    @action="handleMenuAction"
    @clear="clearSelection"
  />

  <div class="toast" :class="{ show: notice }" role="status">{{ notice }}</div>

  <div v-if="showUnlock" class="overlay active" @click.self="showUnlock = false">
    <form class="modal" @submit.prevent="submitUnlock">
      <h2>{{ locale.text('请输入', 'Password required') }}</h2><p>{{ locale.text('此文件夹已锁定，请输入密码：', 'This folder is locked. Enter its password:') }}</p>
      <input v-model="unlockPassword" class="input" type="password" autocomplete="current-password" autofocus>
      <p class="modal-error">{{ unlockError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="showUnlock = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit">{{ locale.t('common.confirm') }}</button></div>
    </form>
  </div>

  <div v-if="showAdmin" class="overlay active" @click.self="showAdmin = false">
    <form class="modal" @submit.prevent="submitAdmin">
      <h2>{{ locale.text('账号登录', 'Account sign-in') }}</h2>
      <label>{{ locale.text('用户名', 'Username') }}<input v-model="adminUser" class="input" autocomplete="username"></label>
      <label>{{ locale.text('密码', 'Password') }}<input v-model="adminPassword" class="input" type="password" autocomplete="current-password"></label>
      <p class="modal-error">{{ adminError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="showAdmin = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit">{{ locale.text('登录', 'Sign in') }}</button></div>
    </form>
  </div>

  <div v-if="showFolder" class="overlay active" @click.self="showFolder = false">
    <form class="modal" @submit.prevent="submitFolder">
      <h2>{{ locale.text('新建文件夹', 'New folder') }}</h2>
      <label>{{ locale.text('名称', 'Name') }}<input v-model="folderName" class="input" autocomplete="off" autofocus></label>
      <p class="modal-error">{{ folderError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="creatingFolder" @click="showFolder = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="creatingFolder">{{ creatingFolder ? locale.text('创建中…', 'Creating…') : locale.t('common.create') }}</button></div>
    </form>
  </div>

  <div v-if="showUpload" class="overlay active" @click.self="!uploading && (showUpload = false)">
    <section class="modal upload-modal" aria-labelledby="upload-title">
      <h2 id="upload-title">{{ locale.text('上传文件', 'Upload files') }}</h2>
      <p class="upload-current">{{ uploadCurrent }}</p>
      <progress :value="uploadPercent" max="100">{{ uploadPercent }}%</progress>
      <p>{{ uploadPercent }}% · {{ formatSize(uploadProcessed) }} / {{ formatSize(uploadTotal) }}</p>
      <p class="upload-summary">{{ uploadSummary }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="uploading" @click="showUpload = false">{{ locale.t('common.close') }}</button></div>
    </section>
  </div>

  <div v-if="showRename" class="overlay active" @click.self="showRename = false">
    <form class="modal" @submit.prevent="submitRename">
      <h2>{{ locale.text('重命名', 'Rename') }}</h2>
      <label>{{ locale.text('新名称', 'New name') }}<input v-model="renameName" class="input" autocomplete="off" autofocus></label>
      <p class="modal-error">{{ renameError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="renaming" @click="showRename = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="renaming">{{ renaming ? locale.t('common.saving') : locale.t('common.save') }}</button></div>
    </form>
  </div>

  <FolderPicker v-if="pickerOperation" :title="pickerTitle" @close="pickerOperation = null" @confirm="confirmTransfer" />

  <div v-if="showDelete" class="overlay active" @click.self="!operationBusy && (showDelete = false)">
    <section class="modal" aria-labelledby="delete-title">
      <h2 id="delete-title">{{ locale.text('确认永久删除', 'Confirm permanent deletion') }}</h2>
      <p>{{ locale.text(`将永久删除 ${pendingDelete.length} 个项目，此操作无法撤销。`, `${pendingDelete.length} item(s) will be permanently deleted. This cannot be undone.`) }}</p>
      <p class="modal-error">{{ operationError }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="operationBusy" @click="showDelete = false">{{ locale.t('common.cancel') }}</button><button class="btn danger" type="button" :disabled="operationBusy" @click="confirmDelete">{{ operationBusy ? locale.text('删除中…', 'Deleting…') : locale.text(`删除 (${pendingDelete.length})`, `Delete (${pendingDelete.length})`) }}</button></div>
    </section>
  </div>

  <div v-if="batchResult" class="overlay active" @click.self="batchResult = null">
    <section class="modal result-modal" aria-labelledby="result-title">
      <h2 id="result-title">{{ locale.text('部分项目未完成', 'Some items were not completed') }}</h2>
      <p>{{ locale.text(`成功 ${batchResult.success} 项，失败 ${batchResult.failed} 项。`, `${batchResult.success} succeeded; ${batchResult.failed} failed.`) }}</p>
      <div class="result-list">
        <div v-for="item in batchResult.results.filter(result => result.status >= 400)" :key="item.path" class="result-row">
          <strong>{{ item.path }}</strong><span>{{ item.message }} ({{ item.code }})</span>
        </div>
      </div>
      <div class="modal-actions"><button class="btn" type="button" @click="batchResult = null">{{ locale.t('common.confirm') }}</button></div>
    </section>
  </div>
</template>

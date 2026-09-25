<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { isPreviewTrafficExhausted, previewUrl, type BrowserCapabilities, type FileEntry } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import AppIcon from '../../shared/components/AppIcon.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import AdminLoginCard from '../../shared/components/AdminLoginCard.vue'
import AppSelect from '../../shared/components/AppSelect.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import BrowserContextMenu from './BrowserContextMenu.vue'
import type { BrowserAction } from './BrowserActionIcon.vue'
import FileIcon from './FileIcon.vue'
import GalleryGrid from './GalleryGrid.vue'
import GalleryLightbox from './GalleryLightbox.vue'
import UserAccountMenu from './UserAccountMenu.vue'
import FolderPicker from './FolderPicker.vue'
import UploadQueueDialog from './UploadQueueDialog.vue'
import { useUploadQueue } from './useUploadQueue'
import { useBrowserSelection } from './useBrowserSelection'
import { useBrowserFileOperations } from './useBrowserFileOperations'
import { useBrowserListing } from './useBrowserListing'
import { useBrowserAccess } from './useBrowserAccess'
import { batchSummary } from './operationFeedback'
import { previewKind } from '../../shared/previewFormats'
import FilePreviewDialog from './FilePreviewDialog.vue'
import MediaPlayer from '../../shared/components/MediaPlayer.vue'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()
const notice = ref('')
const noticeKind = ref<'error' | 'success'>('error')
const noticeRevision = ref(0)
const previewEntry = ref<FileEntry | null>(null)
const pendingDownload = ref<FileEntry | null>(null)
const audioPlayback = ref<{ entry: FileEntry; storageId: string; queue: FileEntry[] } | null>(null)
const audioNav = ref<HTMLDetailsElement | null>(null)
const userAccountMenu = ref<InstanceType<typeof UserAccountMenu>>()
let resetListingSelection = (): void => undefined
let openLockedEntry: (entry: FileEntry) => void = () => undefined
let openRestrictedStorage: (storageId: string) => void = () => undefined

function announce(message: string, kind: 'error' | 'success' = 'error'): void {
  notice.value = message
  noticeKind.value = kind
  noticeRevision.value++
}

const {
  appliedQuery,
  ascending,
  calculateSize,
  calculatingDirectory,
  capabilities,
  changePageSize,
  changeSort,
  crumbs,
  currentStorageId,
  cursorHistory,
  directorySizes,
  disposeListing,
  entries,
  galleryMode,
  isAdministrator,
  loading,
  maxArchiveBytes,
  maxArchiveEntries,
  maxUploadBatchBytes,
  maxUploadBatchEntries,
  maxUploadBytes,
  navigate,
  nextCursor,
  nextPage,
  pageNumber,
  pageSize,
  pageSizeOptions,
  path,
  previousPage,
  query,
  refresh,
  resetAfterSignIn,
  scheduleSearch,
  sort,
  storageOptions,
  storages,
  switchStorage,
  toggleGalleryMode,
  visibleEntries,
} = useBrowserListing({
  announce,
  resetSelection: () => resetListingSelection(),
  openLockedEntry: entry => openLockedEntry(entry),
  requestStorageLogin: storageId => openRestrictedStorage(storageId),
})

const galleryExtensions = new Set(['jpg', 'jpeg', 'png', 'webp', 'avif', 'gif'])
const galleryImages = computed(() => visibleEntries.value.filter(entry => {
  if (entry.is_dir || !entry.name.includes('.')) return false
  return galleryExtensions.has(entry.name.split('.').pop()?.toLowerCase() ?? '')
}))
const activeGalleryPath = ref('')
const activeGalleryIndex = computed(() => galleryImages.value.findIndex(entry => entry.path === activeGalleryPath.value))
const activeGalleryEntry = computed(() => galleryImages.value[activeGalleryIndex.value] ?? null)
const audioIndex = computed(() => audioPlayback.value?.queue.findIndex(entry => entry.path === audioPlayback.value?.entry.path) ?? -1)
const audioSource = computed(() => audioPlayback.value ? previewUrl(audioPlayback.value.entry.path, audioPlayback.value.storageId) : '')

function playAudio(entry: FileEntry): void {
  const queue = visibleEntries.value.filter(item => !item.is_dir && previewKind(item.name) === 'audio')
  audioPlayback.value = { entry, storageId: currentStorageId.value, queue: queue.length ? queue : [entry] }
}

function moveAudio(offset: number): void {
  const playback = audioPlayback.value
  if (!playback) return
  const entry = playback.queue[audioIndex.value + offset]
  if (entry) audioPlayback.value = { ...playback, entry }
}

async function onAudioError(): Promise<void> {
  const playback = audioPlayback.value
  const entry = playback?.entry
  audioPlayback.value = null
  if (!entry || !playback) return
  const trafficExhausted = await isPreviewTrafficExhausted(previewUrl(entry.path, playback.storageId))
  if (audioPlayback.value) return
  if (trafficExhausted) {
    announce(locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.'))
    return
  }
  pendingDownload.value = entry
}

watch(currentStorageId, storageId => {
  if (audioPlayback.value && audioPlayback.value.storageId !== storageId) audioPlayback.value = null
})

function openGalleryEntry(entry: FileEntry): void {
  activeGalleryPath.value = entry.path
}

function closeGalleryEntry(): void {
  activeGalleryPath.value = ''
}

function moveGalleryEntry(offset: number): void {
  const next = activeGalleryIndex.value + offset
  const entry = galleryImages.value[next]
  if (entry) activeGalleryPath.value = entry.path
}

function changeGalleryMode(): void {
  closeGalleryEntry()
  toggleGalleryMode()
}

const {
  adminError,
  adminLoggingIn,
  adminPassword,
  adminTotpCode,
  adminTotpRequired,
  adminUser,
  feedbackRevision,
  onUserSignedIn,
  openAdmin,
  openEntry,
  pendingStorageId,
  requestStorageLogin,
  resetAdminTotpChallenge,
  showAdmin,
  showUnlock,
  signOut,
  submitAdmin,
  submitUnlock,
  unlockError,
  unlockPassword,
} = useBrowserAccess({
  storageId: currentStorageId,
  capabilities,
  isAdministrator,
  navigate,
  resetAfterSignIn,
  openPreviewImage: entry => {
    if (!galleryImages.value.some(image => image.path === entry.path)) return false
    openGalleryEntry(entry)
    return true
  },
  openPreviewFile: entry => {
    const kind = previewKind(entry.name)
    if (kind === 'audio') playAudio(entry)
    else if (kind === 'unsupported') pendingDownload.value = entry
    else {
      if (kind === 'video') audioPlayback.value = null
      previewEntry.value = entry
    }
  },
  openAccountMenu: () => userAccountMenu.value?.open(),
  disposeListing,
})
openLockedEntry = openEntry
openRestrictedStorage = requestStorageLogin

function requireCapability(action: keyof BrowserCapabilities): boolean {
  if (capabilities.value[action]) return true
  announce(locale.text('无权限', 'Permission denied'))
  return false
}

const {
  blockExternalFileDrop,
  chooseFiles,
  chooseFolder,
  clearUploadTasks,
  closeUploadDialog,
  disposeUploads,
  filePanel,
  handleUploadDragEnter,
  handleUploadDragLeave,
  handleUploadDragOver,
  handleUploadDrop,
  openUploadManager,
  pauseUploads,
  removeFailedUpload,
  resumeUploads,
  retryUpload,
  setFileInput,
  setFolderInput,
  showUpload,
  terminateUploads,
  uploadDropActive,
  uploadFiles,
  uploadTasks,
} = useUploadQueue({
  storageId: currentStorageId,
  path,
  maxUploadBytes,
  maxUploadBatchBytes,
  maxUploadBatchEntries,
  canUpload: () => capabilities.value.upload,
  requireUpload: () => requireCapability('upload'),
  announce,
  refresh,
})

const {
  allSelected,
  cancelDragSelection,
  clearSelection,
  closeContextMenu,
  contextEntry,
  contextVisible,
  contextX,
  contextY,
  disposeSelection,
  dragSelecting,
  finishDragSelection,
  handleDocumentClick,
  handleRowClick,
  openBackgroundMenu,
  openRowMenu,
  selected,
  selectedPaths,
  startDragSelection,
  toggleSelectAll,
  toggleSelection,
  updateDragSelection,
} = useBrowserSelection({ entries, visibleEntries, loading, capabilities, filePanel })
resetListingSelection = () => { selected.value = new Set() }

const {
  batchResult,
  confirmDelete,
  confirmTransfer,
  creatingFolder,
  entryForPath,
  folderError,
  folderName,
  openFolderDialog,
  openRenameDialog,
  operationBusy,
  pendingDelete,
  pickerOperation,
  pickerStorageId,
  pickerTitle,
  renameError,
  renameName,
  renaming,
  requestDelete,
  requestTransfer,
  showDelete,
  showFolder,
  showRename,
  startArchive,
  startDownload,
  submitFolder,
  submitRename,
} = useBrowserFileOperations({
  path,
  storageId: currentStorageId,
  entries,
  capabilities,
  maxArchiveBytes,
  maxArchiveEntries,
  selected,
  requireCapability,
  announce,
  refresh,
})

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
      openUploadManager()
      break
    case 'mkdir':
      openFolderDialog()
      break
    case 'folder':
      break
  }
}

function handleEscape(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return
  if (audioNav.value) audioNav.value.open = false
  closeContextMenu()
  if (showUpload.value) closeUploadDialog()
}

function closeAudioNavOnOutsideClick(event: MouseEvent): void {
  if (audioNav.value && !audioNav.value.contains(event.target as Node)) audioNav.value.open = false
}

onMounted(() => {
  void refresh()
  document.addEventListener('click', handleDocumentClick)
  document.addEventListener('click', closeAudioNavOnOutsideClick)
  document.addEventListener('keydown', handleEscape)
  window.addEventListener('mousemove', updateDragSelection, { passive: false })
  window.addEventListener('mouseup', finishDragSelection)
  window.addEventListener('blur', cancelDragSelection)
  window.addEventListener('dragover', blockExternalFileDrop)
  window.addEventListener('drop', blockExternalFileDrop)
})
onBeforeUnmount(() => {
  document.removeEventListener('click', handleDocumentClick)
  document.removeEventListener('click', closeAudioNavOnOutsideClick)
  document.removeEventListener('keydown', handleEscape)
  window.removeEventListener('mousemove', updateDragSelection)
  window.removeEventListener('mouseup', finishDragSelection)
  window.removeEventListener('blur', cancelDragSelection)
  window.removeEventListener('dragover', blockExternalFileDrop)
  window.removeEventListener('drop', blockExternalFileDrop)
  disposeUploads()
  disposeSelection()
  disposeListing()
})
</script>

<template>
  <div class="browser-shell">
    <header class="browser-chrome glass">
      <div class="brand"><AppIcon name="cloud" :size="28" /><span>Ycloud</span></div>
      <div v-if="storages.length" class="storage-switcher browser-storage-switcher">
        <AppSelect :model-value="currentStorageId" :options="storageOptions" :label="locale.text('切换存储', 'Switch storage')" @change="switchStorage" />
      </div>
      <div class="top-actions">
        <button
          class="icon-btn flat gallery-toggle"
          :class="{ active: galleryMode }"
          type="button"
          :aria-pressed="galleryMode"
          :title="galleryMode ? locale.text('返回列表模式', 'Return to list view') : locale.text('打开画廊模式', 'Open gallery view')"
          :aria-label="galleryMode ? locale.text('返回列表模式', 'Return to list view') : locale.text('打开画廊模式', 'Open gallery view')"
          @click="changeGalleryMode"
        >
          <AppIcon :name="galleryMode ? 'list' : 'gallery'" />
        </button>
        <UserAccountMenu ref="userAccountMenu" :storage-name="storages.find(storage => storage.id === currentStorageId)?.name ?? ''" :capabilities="capabilities" @signed-in="onUserSignedIn" @sign-out="signOut" @closed="pendingStorageId = ''" />
        <button class="icon-btn flat" type="button" :title="locale.text('管理员', 'Administrator')" :aria-label="locale.text('管理员', 'Administrator')" @click="openAdmin()"><AppIcon name="administrator" /></button>
        <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
        <LocaleToggle class="flat" />
        <button class="icon-btn flat" type="button" :title="locale.text('退出登录', 'Sign out')" :aria-label="locale.text('退出登录', 'Sign out')" @click="signOut"><AppIcon name="sign-out" /></button>
      </div>
      <input :ref="setFileInput" class="visually-hidden" type="file" multiple @change="uploadFiles">
      <input :ref="setFolderInput" class="visually-hidden" type="file" multiple webkitdirectory directory @change="uploadFiles">
    </header>

    <main class="browser-page" :class="{ 'selection-active': selected.size > 0 }">
      <section
        ref="filePanel"
        class="file-panel glass"
        :class="{ 'drag-selecting': dragSelecting, 'upload-drop-active': uploadDropActive, 'gallery-mode': galleryMode }"
        :aria-busy="loading"
        @mousedown="!galleryMode && startDragSelection($event)"
        @contextmenu="openBackgroundMenu"
        @dragenter="handleUploadDragEnter"
        @dragover="handleUploadDragOver"
        @dragleave="handleUploadDragLeave"
        @drop.stop.prevent="handleUploadDrop"
      >
        <div class="file-toolbar">
          <label class="top-search file-search">
            <AppIcon name="search" />
            <input v-model="query" type="search" :placeholder="locale.text('搜索当前目录', 'Search this folder')" :aria-label="locale.text('搜索当前目录', 'Search this folder')" @input="scheduleSearch">
          </label>
          <div class="file-toolbar-actions">
            <button class="btn" type="button" @click="openFolderDialog"><AppIcon name="folder-plus" /><span>{{ locale.text('新建', 'New') }}</span></button>
            <button class="btn" type="button" @click="openUploadManager"><AppIcon name="upload" /><span>{{ locale.text('上传', 'Upload') }}</span></button>
          </div>
        </div>

        <nav class="breadcrumb" :aria-label="locale.text('当前位置', 'Current path')">
          <button class="crumb home-crumb" :aria-current="crumbs.length ? undefined : 'location'" :aria-label="locale.text('首页', 'Home')" :title="locale.text('首页', 'Home')" type="button" @click="navigate('')"><AppIcon name="home" /></button>
          <template v-for="crumb in crumbs" :key="crumb.path">
            <span class="crumb-separator" aria-hidden="true">›</span>
            <button class="crumb" :aria-current="crumb.path === path ? 'location' : undefined" type="button" @click="navigate(crumb.path)">{{ crumb.label }}</button>
          </template>
        </nav>

        <div v-if="!galleryMode" class="file-head">
          <button class="select-box" :class="{ checked: allSelected }" type="button" :aria-label="locale.text('全选', 'Select all')" @click="toggleSelectAll"><span class="visually-hidden">{{ locale.text('全选', 'Select all') }}</span></button>
          <button class="sort-btn" type="button" @click="changeSort('name')">{{ locale.text('名称', 'Name') }} <span>{{ sort === 'name' ? (ascending ? '▲' : '▼') : '' }}</span></button>
          <button class="sort-btn right" type="button" @click="changeSort('size')">{{ locale.text('大小', 'Size') }} <span>{{ sort === 'size' ? (ascending ? '▲' : '▼') : '' }}</span></button>
          <button class="sort-btn right modified" type="button" @click="changeSort('time')">{{ locale.text('修改时间', 'Modified') }} <span>{{ sort === 'time' ? (ascending ? '▲' : '▼') : '' }}</span></button>
        </div>
        <div v-if="loading" class="empty file-list-body">{{ locale.t('common.loading') }}</div>
        <div v-else-if="!(galleryMode ? galleryImages.length : visibleEntries.length)" class="empty file-list-body">{{ appliedQuery ? locale.text('没有匹配的文件', 'No matching files') : galleryMode ? locale.text('此文件夹没有可展示的图片', 'No supported images in this folder') : locale.text('此文件夹为空', 'This folder is empty') }}</div>
        <GalleryGrid v-else-if="galleryMode" :entries="visibleEntries" :storage-id="currentStorageId" :selected="selected" @open="openGalleryEntry" @select="toggleSelection($event.path)" @context-menu="openRowMenu" />
        <div v-else class="file-list-body">
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
            <div class="cell right">
              <button
                v-if="entry.is_dir" class="directory-size-button" type="button"
                :disabled="calculatingDirectory !== null"
                :aria-busy="calculatingDirectory === entry.path"
                :aria-label="locale.text(`计算 ${entry.name} 的大小`, `Calculate size of ${entry.name}`) + (directorySizes.has(entry.path) ? ': ' + formatSize(directorySizes.get(entry.path)!) : '')"
                :title="locale.text('按需统计文件总大小；点击可重新计算', 'Calculate total file size on demand; click to recalculate')"
                @mousedown.stop @dblclick.stop @click.stop="calculateSize(entry)"
              >
                {{ calculatingDirectory === entry.path ? locale.text('计算中…', 'Calculating…') : directorySizes.has(entry.path) ? formatSize(directorySizes.get(entry.path)!) : locale.text('计算', 'Calculate') }}
              </button>
              <span v-else>{{ formatSize(entry.size) }}</span>
            </div>
            <div class="cell right modified">{{ entry.modified || '-' }}</div>
          </div>
        </div>
        <div v-if="uploadDropActive" class="upload-drop-overlay" aria-hidden="true">
          <AppIcon name="upload" :size="32" />
          <strong>{{ locale.text('拖放到这里上传', 'Drop here to upload') }}</strong>
          <span>{{ locale.text('支持文件和文件夹', 'Files and folders are supported') }}</span>
        </div>
        <footer class="file-pagination">
          <div class="pagination-summary">
            <span v-if="galleryMode" class="gallery-page-size">{{ locale.text('每页 20 张', '20 images per page') }}</span>
            <AppSelect v-else v-model="pageSize" class="page-size-select" placement="top" :options="pageSizeOptions" :label="locale.text('每页显示数量', 'Items per page')" @change="changePageSize" />
          </div>
          <nav class="pagination-controls" :aria-label="locale.text('文件翻页', 'File pagination')">
            <button class="page-arrow" type="button" :disabled="!cursorHistory.length || loading" :title="locale.text('上一页', 'Previous page')" :aria-label="locale.text('上一页', 'Previous page')" @click="previousPage"><span class="page-chevron previous" aria-hidden="true" /></button>
            <span class="current-page" :aria-label="locale.text(`第 ${pageNumber} 页`, `Page ${pageNumber}`)">{{ pageNumber }}</span>
            <button class="page-arrow" type="button" :disabled="!nextCursor || loading" :title="locale.text('下一页', 'Next page')" :aria-label="locale.text('下一页', 'Next page')" @click="nextPage"><span class="page-chevron next" aria-hidden="true" /></button>
          </nav>
        </footer>
      </section>
    </main>
  </div>

  <details v-if="audioPlayback" ref="audioNav" class="ycloud-audio-dock">
    <summary :title="audioPlayback.entry.name" :aria-label="locale.text('展开或收起音乐播放器', 'Expand or collapse music player')">
      <AppIcon name="vinyl-record" :size="28" weight="fill" />
    </summary>
    <div class="ycloud-audio-popover" role="region" :aria-label="locale.text('音乐播放器', 'Music player')">
      <MediaPlayer
        :key="`${audioPlayback.storageId}:${audioPlayback.entry.path}`"
        :src="audioSource"
        :name="audioPlayback.entry.name"
        kind="audio"
        :has-previous="audioIndex > 0"
        :has-next="audioIndex < audioPlayback.queue.length - 1"
        :show-close="true"
        :autoplay="true"
        @previous="moveAudio(-1)"
        @next="moveAudio(1)"
        @close="audioPlayback = null"
        @error="onAudioError"
      />
    </div>
  </details>

  <GalleryLightbox
    v-if="activeGalleryEntry"
    :entry="activeGalleryEntry"
    :storage-id="currentStorageId"
    :index="activeGalleryIndex"
    :total="galleryImages.length"
    @close="closeGalleryEntry"
    @previous="moveGalleryEntry(-1)"
    @next="moveGalleryEntry(1)"
  />

  <FilePreviewDialog v-if="previewEntry" :entry="previewEntry" :storage-id="currentStorageId" @close="previewEntry = null" />

  <ConfirmDialog
    v-if="pendingDownload"
    :title="locale.text('下载文件', 'Download file')"
    :message="locale.text('浏览器无法预览此文件，是否下载？', 'This file cannot be previewed in the browser. Download it?')"
    :target="pendingDownload.name"
    :confirm-label="locale.text('下载', 'Download')"
    @close="pendingDownload = null"
    @confirm="startDownload(pendingDownload.path); pendingDownload = null"
  />

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

  <AppFeedback :message="notice" :kind="noticeKind" :revision="noticeRevision" />

  <div v-if="showUnlock" class="overlay active" @click.self="showUnlock = false">
    <form class="modal short-field-dialog" @submit.prevent="submitUnlock">
      <h2>{{ locale.text('请输入', 'Password required') }}</h2><p>{{ locale.text('此文件夹已锁定，请输入密码：', 'This folder is locked. Enter its password:') }}</p>
      <input v-model="unlockPassword" class="input" type="password" autocomplete="current-password" autofocus>
      <AppFeedback :message="unlockError" :revision="feedbackRevision" />
      <div class="modal-actions"><button class="btn secondary" type="button" @click="showUnlock = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit">{{ locale.t('common.confirm') }}</button></div>
    </form>
  </div>

  <div v-if="showAdmin" class="overlay active admin-login-overlay" @click.self="showAdmin = false">
    <AdminLoginCard
      v-model:username="adminUser"
      v-model:password="adminPassword"
      v-model:totp-code="adminTotpCode"
      :totp-required="adminTotpRequired"
      :error="adminError"
      :busy="adminLoggingIn"
      cancelable
      @credentials-change="resetAdminTotpChallenge"
      @submit="submitAdmin"
      @cancel="showAdmin = false"
    />
  </div>

  <div v-if="showFolder" class="overlay active" @click.self="showFolder = false">
    <form class="modal short-field-dialog" @submit.prevent="submitFolder">
      <h2>{{ locale.text('新建文件夹', 'New folder') }}</h2>
      <label>{{ locale.text('名称', 'Name') }}<input v-model="folderName" class="input" autocomplete="off" autofocus></label>
      <AppFeedback :message="folderError" />
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="creatingFolder" @click="showFolder = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="creatingFolder">{{ creatingFolder ? locale.text('创建中…', 'Creating…') : locale.t('common.create') }}</button></div>
    </form>
  </div>

  <UploadQueueDialog
    v-if="showUpload"
    :tasks="uploadTasks"
    @choose-files="chooseFiles"
    @choose-folder="chooseFolder"
    @drop="handleUploadDrop"
    @close="closeUploadDialog"
    @pause="pauseUploads"
    @resume="resumeUploads"
    @terminate="terminateUploads"
    @clear="clearUploadTasks"
    @retry="retryUpload"
    @remove-failed="removeFailedUpload"
  />

  <div v-if="showRename" class="overlay active" @click.self="showRename = false">
    <form class="modal short-field-dialog" @submit.prevent="submitRename">
      <h2>{{ locale.text('重命名', 'Rename') }}</h2>
      <label>{{ locale.text('新名称', 'New name') }}<input v-model="renameName" class="input" autocomplete="off" autofocus></label>
      <AppFeedback :message="renameError" />
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="renaming" @click="showRename = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="renaming">{{ renaming ? locale.t('common.saving') : locale.t('common.save') }}</button></div>
    </form>
  </div>

  <FolderPicker v-if="pickerOperation" :title="pickerTitle" :storage-id="pickerStorageId" @close="pickerOperation = null" @confirm="confirmTransfer" />

  <ConfirmDialog
    v-if="showDelete"
    :title="locale.text('确认永久删除', 'Confirm permanent deletion')"
    :message="locale.text(`将永久删除 ${pendingDelete.length} 个项目，此操作无法撤销。`, `${pendingDelete.length} item(s) will be permanently deleted. This cannot be undone.`)"
    :busy="operationBusy" @close="showDelete = false" @confirm="confirmDelete"
  />

  <div v-if="batchResult" class="overlay active" @click.self="batchResult = null">
    <section class="modal result-modal" aria-labelledby="result-title">
      <h2 id="result-title">{{ locale.text('部分项目未完成', 'Some items were not completed') }}</h2>
      <p>{{ locale.text(batchSummary(batchResult), batchSummary(batchResult, true)) }}</p>
      <div class="result-list">
        <div v-for="item in batchResult.results.filter(result => result.status >= 400)" :key="item.path" class="result-row">
          <strong>{{ item.path }}</strong><span>{{ item.message }} ({{ item.code }})</span>
        </div>
      </div>
      <div class="modal-actions"><button class="btn" type="button" @click="batchResult = null">{{ locale.t('common.confirm') }}</button></div>
    </section>
  </div>
</template>

<style scoped>
.gallery-toggle.active {
  color: var(--accent);
  background: var(--accent-soft);
  border-color: color-mix(in srgb, var(--accent) 28%, var(--line));
}
.gallery-page-size {
  display: inline-flex;
  align-items: center;
  min-height: 34px;
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
}
.file-panel.gallery-mode { overflow-y: auto; }
.ycloud-audio-dock { position: fixed; z-index: 80; bottom: clamp(90px, 27vh, 290px); left: clamp(14px, 1.8vw, 34px); width: 56px; height: 56px; }
.ycloud-audio-dock summary { display: grid; place-items: center; width: 56px; height: 56px; color: var(--audio-accent); background: var(--panel); border: 1px solid var(--line); border-radius: 50%; box-shadow: 0 4px 16px rgb(0 0 0 / 10%); cursor: pointer; list-style: none; }
.ycloud-audio-dock summary::-webkit-details-marker { display: none; }
.ycloud-audio-dock summary:hover, .ycloud-audio-dock[open] summary { background: var(--audio-accent-soft); border-color: var(--audio-accent); }
.ycloud-audio-dock summary :deep(.app-icon) { color: currentColor !important; }
.ycloud-audio-dock summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
.ycloud-audio-popover { position: absolute; bottom: 0; left: calc(100% + 12px); width: min(380px, calc(100vw - 120px)); padding: 8px; background: var(--panel); border: 1px solid var(--line); border-radius: 11px; box-shadow: 0 12px 30px rgb(0 0 0 / 12%); }
.ycloud-audio-popover :deep(.ycloud-media-player.audio) { width: 100%; max-width: none; background: transparent; border: 0; }
@media (max-width: 520px) { .ycloud-audio-popover { bottom: calc(100% + 10px); left: 0; width: calc(100vw - 28px); } }
</style>

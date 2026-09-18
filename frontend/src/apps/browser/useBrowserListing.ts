import { computed, ref } from 'vue'
import type { BrowserCapabilities, BrowserStorage, FileEntry } from '../../shared/api/browser'
import { calculateDirectorySize, listFiles, listStorages } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'

type SortKey = 'name' | 'time' | 'size'
type PageSize = 10 | 20 | 50 | 100

interface BrowserListingContext {
  announce: (message: string) => void
  resetSelection: () => void
  openLockedEntry: (entry: FileEntry) => void
  requestStorageLogin: (storageId: string) => void
}

const PAGE_SIZES: PageSize[] = [10, 20, 50, 100]

export function useBrowserListing(context: BrowserListingContext) {
  const locale = useLocale()
  const path = ref('')
  const currentStorageId = ref('')
  const storages = ref<BrowserStorage[]>([])
  const entries = ref<FileEntry[]>([])
  const directorySizes = ref(new Map<string, number>())
  const calculatingDirectory = ref<string | null>(null)
  const query = ref('')
  const appliedQuery = ref('')
  const sort = ref<SortKey>('name')
  const ascending = ref(true)
  const pageSize = ref<PageSize>(20)
  const currentCursor = ref<string>()
  const nextCursor = ref<string | null>(null)
  const cursorHistory = ref<Array<string | undefined>>([])
  const loading = ref(true)
  const isAdministrator = ref(false)
  const capabilities = ref<BrowserCapabilities>({ download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false })
  const maxUploadBytes = ref(0)
  const maxUploadBatchBytes = ref(0)
  const maxUploadBatchEntries = ref(0)
  const maxArchiveBytes = ref(0)
  const maxArchiveEntries = ref(0)
  let directorySizeRequest: AbortController | undefined
  let directorySizeSequence = 0
  let searchTimer: number | undefined
  let refreshSequence = 0

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

  function resetDirectorySizes(): void {
    directorySizeSequence++
    directorySizeRequest?.abort()
    directorySizeRequest = undefined
    calculatingDirectory.value = null
    directorySizes.value = new Map()
  }

  function resetPagination(): void {
    currentCursor.value = undefined
    nextCursor.value = null
    cursorHistory.value = []
    context.resetSelection()
  }

  async function refresh(): Promise<void> {
    resetDirectorySizes()
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
      maxUploadBatchBytes.value = data.max_upload_batch_bytes ?? data.max_upload_bytes
      maxUploadBatchEntries.value = data.max_upload_batch_entries ?? 1
      maxArchiveBytes.value = data.max_archive_bytes
      maxArchiveEntries.value = data.max_archive_entries
      context.resetSelection()
    } catch (error) {
      if (sequence !== refreshSequence) return
      if (!storages.value.length) {
        try { storages.value = await listStorages() } catch { /* Keep the original file-list error. */ }
      }
      context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
    } finally {
      if (sequence === refreshSequence) loading.value = false
    }
  }

  async function calculateSize(entry: FileEntry): Promise<void> {
    if (!entry.is_dir || calculatingDirectory.value !== null || loading.value) return
    if (entry.locked) { context.openLockedEntry(entry); return }
    directorySizes.value.delete(entry.path)
    const sequence = ++directorySizeSequence
    const controller = new AbortController()
    directorySizeRequest = controller
    calculatingDirectory.value = entry.path
    try {
      const result = await calculateDirectorySize(entry.path, currentStorageId.value, controller.signal)
      if (sequence !== directorySizeSequence) return
      if (!Number.isFinite(result.size) || result.size < 0) throw new Error(locale.t('common.invalidResponse'))
      directorySizes.value.set(entry.path, result.size)
    } catch (error) {
      if (sequence !== directorySizeSequence || controller.signal.aborted) return
      context.announce(error instanceof Error ? error.message : locale.text('目录大小计算失败', 'Unable to calculate folder size'))
    } finally {
      if (sequence === directorySizeSequence) {
        calculatingDirectory.value = null
        directorySizeRequest = undefined
      }
    }
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
      context.requestStorageLogin(nextStorageId)
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
    context.resetSelection()
    void refresh()
  }

  function previousPage(): void {
    if (!cursorHistory.value.length || loading.value) return
    const history = [...cursorHistory.value]
    currentCursor.value = history.pop()
    cursorHistory.value = history
    context.resetSelection()
    void refresh()
  }

  async function resetAfterSignIn(requestedStorageId: string): Promise<void> {
    resetDirectorySizes()
    ++refreshSequence
    if (searchTimer !== undefined) window.clearTimeout(searchTimer)
    entries.value = []
    capabilities.value = { download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false }
    isAdministrator.value = false
    const available = await listStorages().catch(() => [])
    storages.value = available
    const requested = available.find(storage => storage.id === requestedStorageId && !storage.requires_login)
    const current = available.find(storage => storage.id === currentStorageId.value && !storage.requires_login)
    currentStorageId.value = requested?.id ?? current?.id ?? available.find(storage => !storage.requires_login)?.id ?? ''
    path.value = ''
    query.value = ''
    appliedQuery.value = ''
    resetPagination()
    await refresh()
  }

  function disposeListing(): void {
    resetDirectorySizes()
    if (searchTimer !== undefined) window.clearTimeout(searchTimer)
  }

  return {
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
    visibleEntries,
  }
}

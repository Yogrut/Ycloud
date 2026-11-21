import { computed, ref } from 'vue'
import type { BrowserCapabilities, BrowserStorage, FileEntry, FileListResponse } from '../../shared/api/browser'
import { calculateDirectorySize, listFiles, listStorages } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useLocale } from '../../shared/i18n'
import { isStorageUsable, rememberedStorage, rememberStorage, shouldForgetStorage } from './storageSelection'

type SortKey = 'name' | 'time' | 'size'
type PageSize = 10 | 20 | 50 | 100

interface PageTarget {
  cursor: string | undefined
  history: Array<string | undefined>
}

interface ListingRefresh {
  target?: PageTarget
  keepGallery?: boolean
  fillAfterDelete?: boolean
  /// Must run even while a visible refresh is in flight and ignores the minimum
  /// interval: used after a write, where a fresh list is the only proof.
  force?: boolean
  /// Refresh without a loading state, without touching the selection and
  /// without announcing transient failures.
  silent?: boolean
}

interface BrowserListingContext {
  announce: (message: string) => void
  resetSelection: () => void
  /// Drops selected paths that are no longer in the list.
  pruneSelection: (keep: Set<string>) => void
  openLockedEntry: (entry: FileEntry) => void
  requestStorageLogin: (storageId: string) => void
}

const PAGE_SIZES: PageSize[] = [10, 20, 50, 100]
const SEARCH_DELAY_MS = 250

function emptyCapabilities(): BrowserCapabilities {
  return {
    download: false,
    upload: false,
    create_directory: false,
    rename: false,
    move_items: false,
    copy: false,
    delete: false,
  }
}

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
  const galleryMode = ref(false)
  const currentCursor = ref<string>()
  const nextCursor = ref<string | null>(null)
  const cursorHistory = ref<Array<string | undefined>>([])
  const loading = ref(true)
  const retainingGallery = ref(false)
  const emptyReason = ref<string | null>(null)
  let selectionScope = ''
  let restoreSelection = true
  let rememberNextSelection = false
  const isAdministrator = ref(false)
  const capabilities = ref<BrowserCapabilities>(emptyCapabilities())
  const maxUploadBytes = ref(0)
  const maxUploadBatchBytes = ref(0)
  const maxUploadBatchEntries = ref(0)
  const maxArchiveBytes = ref(0)
  const maxArchiveEntries = ref(0)
  let directorySizeRequest: AbortController | undefined
  let directorySizeSequence = 0
  let searchTimer: number | undefined
  let refreshSequence = 0
  let listingRequest: AbortController | undefined

  const visibleEntries = computed(() => entries.value)
  const pageNumber = computed(() => cursorHistory.value.length + 1)
  const storageOptions = computed(() => storages.value.map(storage => ({
    value: storage.id,
    label: `${storage.name}${storage.enabled === false ? locale.text('（停用）', ' (disabled)') : storage.ready === false ? locale.text('（异常）', ' (unavailable)') : ''}`,
    disabled: !isStorageUsable(storage),
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

  function setGalleryMode(enabled: boolean): void {
    if (galleryMode.value === enabled) return
    galleryMode.value = enabled
    resetPagination()
  }

  function clearListingEntries(): void {
    entries.value = []
    capabilities.value = emptyCapabilities()
    setGalleryMode(false)
    pendingRemovals.clear()
  }

  /// Rows hidden while their delete/move is still in flight. Keyed by storage so
  /// another storage that happens to share a path is never affected.
  /// Hidden rows, keyed by storage + path. `null` = operation still running;
  /// a number = the monotonic ticket taken when the operation settled. A listing
  /// started after that ticket may show the row again (or keep it hidden if the
  /// server no longer has it).
  const pendingRemovals = new Map<string, number | null>()
  /// Monotonic ticket source, incremented when a listing request starts.
  let startCounter = 0
  let settledTimers: number[] = []
  /// When the current page last came from the server; drives the visibility check.
  let lastLoadedAt = 0
  /// Silent refreshes run one at a time and at most once per interval: every
  /// write invalidates the server snapshot, so a burst must not turn into a
  /// request storm.
  const SILENT_MIN_INTERVAL_MS = 1500
  let silentInFlight = false
  let silentQueued = false
  let forceQueued = false
  let silentTimer: number | undefined
  let lastSilentAt = 0
  const pendingRemovalKey = (storageId: string, path: string): string =>
    `${storageId}\u0000${path.replace(/^\/+|\/+$/g, '')}`

  /// Hides rows at once so the user sees the change immediately; the request then
  /// runs in the background.
  function hideEntries(storageId: string, paths: string[]): void {
    if (storageId !== currentStorageId.value) return
    const hidden = new Set(paths.map(path => path.replace(/^\/+|\/+$/g, '')))
    for (const path of hidden) pendingRemovals.set(pendingRemovalKey(storageId, path), null)
    if (!entries.value.some(entry => hidden.has(entry.path.replace(/^\/+|\/+$/g, '')))) return
    entries.value = entries.value.filter(entry => !hidden.has(entry.path.replace(/^\/+|\/+$/g, '')))
    for (const path of hidden) directorySizes.value.delete(path)
  }

  /// Stops hiding; call from `finally` so timeouts and exceptions release too.
  function releaseEntries(storageId: string, paths: string[]): void {
    for (const path of paths) pendingRemovals.delete(pendingRemovalKey(storageId, path))
  }

  /// The operation finished: keep its rows hidden until a listing that started
  /// after this ticket is applied. Releasing here would let an older response
  /// (sent before the write) put the rows back.
  function settleEntries(storageId: string, paths: string[]): void {
    const settledAt = ++startCounter
    for (const path of paths) {
      const key = pendingRemovalKey(storageId, path)
      if (pendingRemovals.has(key)) pendingRemovals.set(key, settledAt)
    }
    // Never hide a row forever: if no listing ever arrives, give up after 10s.
    settledTimers.push(window.setTimeout(() => {
      for (const path of paths) pendingRemovals.delete(pendingRemovalKey(storageId, path))
    }, 10_000))
  }

  function applyListing(data: FileListResponse, preserveSelection = false, startedAt = 0): void {
    lastLoadedAt = Date.now()
    emptyReason.value = data.empty_reason ?? null
    currentStorageId.value = data.storage_id
    storages.value = data.storages ?? []
    path.value = data.current_path.replace(/^\/+|\/+$/g, '')
    const kept: FileEntry[] = []
    for (const entry of data.entries) {
      const key = pendingRemovalKey(data.storage_id, entry.path)
      const settledAt = pendingRemovals.get(key)
      if (settledAt === undefined) {
        kept.push(entry)
        continue
      }
      // Still running, or started before the operation settled: keep it hidden.
      if (settledAt === null || settledAt >= startedAt) continue
      // This listing started after the operation settled and the server still
      // returns the row, so it is real again.
      pendingRemovals.delete(key)
      kept.push(entry)
    }
    entries.value = kept
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
    if (preserveSelection) context.pruneSelection(new Set(entries.value.map(entry => entry.path)))
    else context.resetSelection()
  }

  async function refresh({ target, keepGallery = false, fillAfterDelete = false, silent = false, force = false }: ListingRefresh = {}): Promise<void> {
    if (silent && !force) {
      // A visible refresh (navigation, paging, sorting) always wins: a silent one
      // must never abort it.
      if (loading.value) return
      if (silentInFlight) {
        silentQueued = true
        return
      }
      const wait = SILENT_MIN_INTERVAL_MS - (Date.now() - lastSilentAt)
      if (wait > 0) {
        if (silentTimer === undefined) {
          silentTimer = window.setTimeout(() => {
            silentTimer = undefined
            void refresh({ silent: true })
          }, wait)
        }
        return
      }
      silentInFlight = true
      lastSilentAt = Date.now()
    } else if (silent) {
      // Forced: skips the minimum interval, but never cancels a visible refresh —
      // it queues and runs as soon as that one finishes.
      if (loading.value || silentInFlight) {
        forceQueued = true
        return
      }
      silentInFlight = true
      lastSilentAt = Date.now()
    } else {
      silentQueued = false
      if (silentTimer !== undefined) {
        window.clearTimeout(silentTimer)
        silentTimer = undefined
      }
    }
    resetDirectorySizes()
    const startedAt = ++startCounter
    const sequence = ++refreshSequence
    listingRequest?.abort()
    const controller = new AbortController()
    listingRequest = controller
    retainingGallery.value = keepGallery && galleryMode.value
    if (!silent) loading.value = true
    try {
      const data = await listFiles(path.value, currentStorageId.value || undefined, {
        signal: controller.signal,
        limit: pageSize.value,
        cursor: target ? target.cursor : currentCursor.value,
        search: appliedQuery.value || undefined,
        sort: sort.value,
        direction: ascending.value ? 'asc' : 'desc',
        gallery: galleryMode.value || undefined,
      })
      if (sequence !== refreshSequence) return
      const scope = data.selection_scope ?? ''
      const available = data.storages ?? []
      if (restoreSelection || scope !== selectionScope) {
        restoreSelection = false
        selectionScope = scope
        const saved = scope ? rememberedStorage(scope) : ''
        const target = available.find(storage => storage.id === saved && isStorageUsable(storage))
        const savedStorage = available.find(storage => storage.id === saved)
        if (saved && shouldForgetStorage(savedStorage)) rememberStorage(scope, '')
        if (target && target.id !== data.storage_id) {
          currentStorageId.value = target.id
          path.value = ''
          resetPagination()
          await refresh()
          return
        }
      }
      if (rememberNextSelection) {
        if (scope) rememberStorage(scope, data.storage_id)
        rememberNextSelection = false
      }
      if (galleryMode.value && data.capabilities?.download === false) {
        clearListingEntries()
        // A gallery cursor and its image-only results cannot be reused by the full list.
        if (!data.empty_reason) {
          await refresh()
          return
        }
        applyListing(data, silent, startedAt)
        return
      }
      if (fillAfterDelete && !data.empty_reason && !data.entries.length && cursorHistory.value.length) {
        await refresh({ target: previousTarget(), keepGallery: true })
        return
      }
      if (target) {
        currentCursor.value = target.cursor
        cursorHistory.value = target.history
      }
      applyListing(data, silent, startedAt)
    } catch (error) {
      if (sequence !== refreshSequence) return
      if (error instanceof ApiError && ([401, 403].includes(error.status)
        || !silent && !target && !keepGallery && [404, 503].includes(error.status))) {
        clearListingEntries()
        context.resetSelection()
      }
      if (!storages.value.length || error instanceof ApiError && (error.status === 403 || error.status === 404 || error.status === 503)) {
        try {
          const available = await listStorages(controller.signal)
          if (sequence !== refreshSequence) return
          storages.value = available
          const current = available.find(storage => storage.id === currentStorageId.value)
          if (currentStorageId.value && !isStorageUsable(current)) {
            entries.value = []
            currentStorageId.value = ''
            path.value = ''
            resetPagination()
            rememberNextSelection = false
            if (shouldForgetStorage(current) && selectionScope) rememberStorage(selectionScope, '')
            await refresh()
            return
          }
        } catch { /* Keep the original file-list error. */ }
      }
      if (sequence !== refreshSequence) return
      if (!silent) context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
    } finally {
      if (silent) {
        silentInFlight = false
        if (forceQueued) {
          forceQueued = false
          silentQueued = false
          void refresh({ silent: true, force: true })
        } else if (silentQueued) {
          silentQueued = false
          void refresh({ silent: true })
        }
      }
      if (sequence === refreshSequence) {
        loading.value = false
        retainingGallery.value = false
        listingRequest = undefined
      }
    }
  }

  /// Refreshes silently when the page has gone stale, e.g. after the tab was in
  /// the background. Writes made outside the app are only visible this way.
  async function refreshIfStale(maxAgeMs: number): Promise<void> {
    if (document.visibilityState === 'hidden') return
    if (Date.now() - lastLoadedAt < maxAgeMs) return
    await refresh({ silent: true })
  }

  async function calculateSize(entry: FileEntry): Promise<void> {
    if (!entry.is_dir || calculatingDirectory.value !== null || loading.value) return
    if (entry.locked) {
      context.openLockedEntry(entry)
      return
    }
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
    }, SEARCH_DELAY_MS)
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
    if (nextStorage && !isStorageUsable(nextStorage)) return
    if (!nextStorage) {
      context.requestStorageLogin(nextStorageId)
      return
    }
    rememberNextSelection = true
    clearListingEntries()
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

  function toggleGalleryMode(): void {
    if (!capabilities.value.download) return
    setGalleryMode(!galleryMode.value)
    void refresh()
  }

  function nextPage(): void {
    if (!nextCursor.value || loading.value) return
    void refresh({
      target: { cursor: nextCursor.value, history: [...cursorHistory.value, currentCursor.value] },
      keepGallery: true,
    })
  }

  function previousTarget(): PageTarget {
    const history = [...cursorHistory.value]
    return { cursor: history.pop(), history }
  }

  function previousPage(): void {
    if (!cursorHistory.value.length || loading.value) return
    void refresh({ target: previousTarget(), keepGallery: true })
  }

  async function reconcileDeletion(storageId: string, paths: string[]): Promise<void> {
    if (storageId !== currentStorageId.value) return
    const deleted = new Set(paths.map(path => path.replace(/^\/+|\/+$/g, '')))
    const remaining = entries.value.filter(entry => !deleted.has(entry.path.replace(/^\/+|\/+$/g, '')))
    // Drop the removed rows at once, then fill the page in the background. The
    // list is never blanked, so no loading state can flash between the two.
    if (remaining.length !== entries.value.length) entries.value = remaining
    // Always fetch a fresh page after a removal. The server snapshot may still
    // contain the removed rows, and only a newer response replaces an older one;
    // releasing the hidden paths before that would resurrect them.
    await refresh({ keepGallery: true, fillAfterDelete: true, silent: true, force: true })
  }

  async function resetAfterSignIn(requestedStorageId: string): Promise<void> {
    resetDirectorySizes()
    const sequence = ++refreshSequence
    listingRequest?.abort()
    const controller = new AbortController()
    listingRequest = controller
    if (searchTimer !== undefined) window.clearTimeout(searchTimer)
    clearListingEntries()
    isAdministrator.value = false
    const available = await listStorages(controller.signal).catch(() => [])
    if (sequence !== refreshSequence) return
    storages.value = available
    const requested = available.find(storage => storage.id === requestedStorageId)
    restoreSelection = true
    selectionScope = ''
    rememberNextSelection = false
    currentStorageId.value = requested && isStorageUsable(requested) ? requested.id : ''
    path.value = ''
    query.value = ''
    appliedQuery.value = ''
    resetPagination()
    await refresh()
  }

  function disposeListing(): void {
    ++refreshSequence
    listingRequest?.abort()
    listingRequest = undefined
    resetDirectorySizes()
    if (searchTimer !== undefined) window.clearTimeout(searchTimer)
    if (silentTimer !== undefined) window.clearTimeout(silentTimer)
    silentTimer = undefined
    for (const timer of settledTimers) window.clearTimeout(timer)
    settledTimers = []
    silentQueued = false
    forceQueued = false
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
    emptyReason,
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
    hideEntries,
    releaseEntries,
    settleEntries,
    refreshIfStale,
    reconcileDeletion,
    refresh,
    retainingGallery,
    resetAfterSignIn,
    scheduleSearch,
    sort,
    storageOptions,
    storages,
    switchStorage,
    toggleGalleryMode,
    visibleEntries,
  }
}

import { afterEach, describe, expect, it, vi } from 'vitest'
import * as api from '../../shared/api/browser'
import type { BrowserCapabilities, BrowserStorage, FileEntry, FileListResponse } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useBrowserListing } from './useBrowserListing'

const storages: BrowserStorage[] = ['first', 'second'].map(id => ({ id, name: id, enabled: true, ready: true }))
function response(id = 'first', scope = 'guest', items = storages): FileListResponse {
  return { storage_id: id, selection_scope: scope, storages: items, current_path: '', parent_path: null, entries: [], page_start: 0, page_size: 20, next_cursor: null, can_write: false, max_upload_bytes: 1, max_archive_bytes: 1, max_archive_entries: 1 }
}
function listing() { return useBrowserListing({ announce: vi.fn(), resetSelection: vi.fn(), pruneSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin: vi.fn() }) }
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

const writable: BrowserCapabilities = { download: true, upload: true, create_directory: true, rename: true, move_items: true, copy: true, delete: true }
const entry: FileEntry = { name: 'one.txt', path: 'folder/one.txt', is_dir: false, size: 3, modified: '2026-09-30T00:00:00Z', mime: 'text/plain', icon: 'file', locked: false }
afterEach(() => { vi.restoreAllMocks(); localStorage.clear() })

describe('silent refresh', () => {
  function silentListing() {
    const context = { announce: vi.fn(), resetSelection: vi.fn(), pruneSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin: vi.fn() }
    return { context, state: useBrowserListing(context) }
  }

  it('never shows a loading state and keeps the selection instead of resetting it', async () => {
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue({ ...response(), entries: [entry] })
    const { context, state } = silentListing()
    await state.refresh()
    expect(context.resetSelection).toHaveBeenCalledTimes(1)
    await state.refresh({ silent: true })
    expect(state.loading.value).toBe(false)
    expect(request).toHaveBeenCalledTimes(2)
    expect(context.resetSelection).toHaveBeenCalledTimes(1)
    expect(context.pruneSelection).toHaveBeenCalledWith(new Set([entry.path]))
    state.disposeListing()
  })

  it('lets a visible refresh finish instead of cancelling it', async () => {
    const pending = deferred<FileListResponse>()
    const request = vi.spyOn(api, 'listFiles').mockReturnValueOnce(pending.promise).mockResolvedValue(response())
    const { state } = silentListing()
    const visible = state.refresh()
    await state.refresh({ silent: true })
    expect(request).toHaveBeenCalledTimes(1)
    pending.resolve(response())
    await visible
    state.disposeListing()
  })

  it('does not resurrect a deleted row from an older response', async () => {
    const slow = deferred<FileListResponse>()
    const listed = { ...response(), entries: [entry] }
    vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(listed)
      .mockReturnValueOnce(slow.promise)
      .mockResolvedValue({ ...response(), entries: [] })
    const { state } = silentListing()
    await state.refresh()
    expect(state.entries.value).toHaveLength(1)
    const inFlight = state.refresh({ silent: true })
    state.hideEntries('first', [entry.path])
    await state.reconcileDeletion('first', [entry.path])
    // The batch releases its hidden paths once it is done; from here only a newer
    // listing may keep the old response from being applied.
    state.releaseEntries('first', [entry.path])
    slow.resolve(listed)
    await inFlight
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(state.entries.value).toHaveLength(0)
    state.disposeListing()
  })

  it('collapses a burst of silent refreshes into one request', async () => {
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue(response())
    const { state } = silentListing()
    await state.refresh()
    await state.refresh({ silent: true })
    await state.refresh({ silent: true })
    await state.refresh({ silent: true })
    expect(request).toHaveBeenCalledTimes(2)
    state.disposeListing()
  })
})

describe('gallery page transactions', () => {
  const settle = () => new Promise(resolve => setTimeout(resolve, 0))
  const photo = (name: string): FileEntry => ({ ...entry, name, path: `photos/${name}`, mime: 'image/jpeg', icon: 'image' })

  it('uses the selected gallery size and clears the preceding page cursor on changes', async () => {
    const request = vi.spyOn(api, 'listFiles').mockImplementation(async (_path, _storage, options) => ({
      ...response(), entries: Array.from({ length: options?.limit ?? 20 }, (_, index) => photo(`${index}.jpg`)),
      next_cursor: 'next-page',
    }))
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    state.nextPage()
    await settle()
    expect(state.pageNumber.value).toBe(2)
    for (const size of [10, 20, 50, 100] as const) {
      state.pageSize.value = size
      state.changePageSize()
      await settle()
      expect(state.pageNumber.value).toBe(1)
      expect(request).toHaveBeenLastCalledWith('', 'first', expect.objectContaining({ limit: size, gallery: true, cursor: undefined }))
      expect(state.entries.value).toHaveLength(size)
    }
    state.toggleGalleryMode()
    await settle()
    expect(request).toHaveBeenLastCalledWith('', 'first', expect.objectContaining({ limit: 100, gallery: undefined }))
  })

  it('keeps the original entries, cursor and page number on a failed turn', async () => {
    const first = photo('first.jpg')
    const second = photo('second.jpg')
    const pending = deferred<FileListResponse>()
    const request = vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [first], next_cursor: 'second-page' })
      .mockReturnValueOnce(pending.promise)
      .mockResolvedValueOnce({ ...response(), entries: [second] })
    vi.spyOn(api, 'listStorages').mockResolvedValue(storages)
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    state.nextPage()
    expect(state.pageNumber.value).toBe(1)
    expect(state.entries.value).toEqual([first])
    expect(state.retainingGallery.value).toBe(true)
    pending.reject(new ApiError('Unavailable', 503))
    await settle()
    expect(state.pageNumber.value).toBe(1)
    expect(state.nextCursor.value).toBe('second-page')
    expect(state.entries.value).toEqual([first])
    state.nextPage()
    await settle()
    expect(request).toHaveBeenLastCalledWith('', 'first', expect.objectContaining({ cursor: 'second-page' }))
    expect(state.pageNumber.value).toBe(2)
    expect(state.entries.value).toEqual([second])
  })

  it('removes only confirmed files and retains the remaining page while filling it', async () => {
    const removed = photo('remove.jpg')
    const kept = photo('keep.jpg')
    const replacement = photo('fill.jpg')
    const refill = deferred<FileListResponse>()
    const request = vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [removed, kept], next_cursor: 'next-page' })
      .mockReturnValueOnce(refill.promise)
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    await state.reconcileDeletion('second', ['/photos/remove.jpg'])
    expect(state.entries.value).toEqual([removed, kept])
    expect(request).toHaveBeenCalledTimes(2)
    const deletion = state.reconcileDeletion('first', ['/photos/remove.jpg'])
    expect(state.entries.value).toEqual([kept])
    expect(state.retainingGallery.value).toBe(true)
    refill.resolve({ ...response(), entries: [kept, replacement] })
    await deletion
    expect(state.pageNumber.value).toBe(1)
    expect(state.entries.value).toEqual([kept, replacement])
  })

  it('returns to the preceding page after deleting the last image of the last page', async () => {
    const first = photo('first.jpg')
    const last = photo('last.jpg')
    const request = vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [first], next_cursor: 'last-page' })
      .mockResolvedValueOnce({ ...response(), entries: [last] })
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [first] })
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    state.nextPage()
    await settle()
    expect(state.pageNumber.value).toBe(2)
    await state.reconcileDeletion('first', ['/photos/last.jpg'])
    expect(request.mock.calls[3]?.[2]?.cursor).toBe('last-page')
    expect(request.mock.calls[4]?.[2]?.cursor).toBeUndefined()
    expect(state.pageNumber.value).toBe(1)
    expect(state.entries.value).toEqual([first])
  })

  it('leaves the gallery and reloads the full first page when download permission is revoked', async () => {
    const capabilities = { ...writable, download: false }
    const request = vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [photo('first.jpg')], next_cursor: 'gallery-next' })
      .mockResolvedValueOnce({ ...response(), entries: [photo('second.jpg')], capabilities })
      .mockResolvedValueOnce({ ...response(), entries: [entry], capabilities })
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    state.nextPage()
    await settle()
    expect(request).toHaveBeenNthCalledWith(3, '', 'first', expect.objectContaining({ gallery: true, cursor: 'gallery-next' }))
    expect(request).toHaveBeenNthCalledWith(4, '', 'first', expect.objectContaining({ gallery: undefined, cursor: undefined }))
    expect(state.galleryMode.value).toBe(false)
    expect(state.pageNumber.value).toBe(1)
    expect(state.entries.value).toEqual([entry])
    state.toggleGalleryMode()
    expect(request).toHaveBeenCalledTimes(4)
  })

  it('does not carry gallery mode across a switch to a storage without download permission', async () => {
    const request = vi.spyOn(api, 'listFiles')
      .mockResolvedValueOnce(response())
      .mockResolvedValueOnce({ ...response(), entries: [photo('first.jpg')] })
      .mockResolvedValueOnce({ ...response('second'), entries: [entry], capabilities: { ...writable, download: false } })
    const state = listing()
    await state.refresh()
    state.toggleGalleryMode()
    await settle()
    expect(state.galleryMode.value).toBe(true)
    await state.switchStorage('second')
    expect(request).toHaveBeenLastCalledWith('', 'second', expect.objectContaining({ gallery: undefined }))
    expect(state.galleryMode.value).toBe(false)
    expect(state.entries.value).toEqual([entry])
  })
})

describe('storage selection', () => {
  it('starts with the first available storage and remembers manual selection', async () => {
    vi.spyOn(api, 'listFiles').mockImplementation(async (_path, id) => response(id))
    const state = listing()
    await state.refresh()
    expect(state.currentStorageId.value).toBe('first')
    await state.switchStorage('second')
    expect(localStorage.getItem('ycloud:storage:guest')).toBe('second')
    const restored = listing()
    await restored.refresh()
    expect(restored.currentStorageId.value).toBe('second')
  })
  it('isolates guest and account choices', async () => {
    localStorage.setItem('ycloud:storage:guest', 'second')
    vi.spyOn(api, 'listStorages').mockResolvedValue(storages)
    const request = vi.spyOn(api, 'listFiles').mockImplementation(async (_path, id) => response(id, 'user:alice'))
    const state = listing()
    await state.refresh()
    expect(state.currentStorageId.value).toBe('first')
    await state.switchStorage('second')
    expect(localStorage.getItem('ycloud:storage:user:alice')).toBe('second')
    request.mockImplementation(async (_path, id) => response(id, 'user:bob'))
    await state.resetAfterSignIn('')
    expect(state.currentStorageId.value).toBe('first')
  })
  it('opens account login for a storage missing from the authorized list', async () => {
    const requestStorageLogin = vi.fn()
    const state = useBrowserListing({ announce: vi.fn(), resetSelection: vi.fn(), pruneSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin })
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue(response())
    await state.refresh()
    await state.switchStorage('private')
    expect(requestStorageLogin).toHaveBeenCalledWith('private')
    expect(request).toHaveBeenCalledTimes(1)
    state.disposeListing()
  })

  it('keeps a temporarily unavailable preference but drops a revoked one', async () => {
    localStorage.setItem('ycloud:storage:guest', 'second')
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue(response('first', 'guest', [storages[0]!, { ...storages[1]!, ready: false }]))
    await listing().refresh()
    expect(localStorage.getItem('ycloud:storage:guest')).toBe('second')
    request.mockResolvedValue(response('first', 'guest', [storages[0]!]))
    await listing().refresh()
    expect(localStorage.getItem('ycloud:storage:guest')).toBeNull()
  })
  it('clears revoked entries and falls back without overwriting another preference', async () => {
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue(response())
    vi.spyOn(api, 'listStorages').mockResolvedValue([storages[1]!])
    const state = listing()
    await state.refresh()
    request.mockRejectedValueOnce(new ApiError('denied', 403)).mockResolvedValue(response('second', 'guest', [storages[1]!]))
    await state.refresh()
    expect(state.currentStorageId.value).toBe('second')
    expect(state.entries.value).toEqual([])
  })
  it('does not request disabled storage and supports a zero-storage response', async () => {
    const request = vi.spyOn(api, 'listFiles').mockResolvedValue({ ...response('', 'administrator', [{ ...storages[0]!, enabled: false }]), empty_reason: 'unavailable' })
    const state = listing()
    await state.refresh()
    await state.switchStorage('first')
    expect(request).toHaveBeenCalledTimes(1)
    expect(state.emptyReason.value).toBe('unavailable')
    request.mockResolvedValue({ ...response('', 'administrator', []), empty_reason: 'unconfigured' })
    await state.refresh()
    expect(state.emptyReason.value).toBe('unconfigured')
    expect(state.currentStorageId.value).toBe('')
  })
})

describe('listing response application', () => {
  it('applies server capabilities and limits without falling back to can_write', async () => {
    const permissions = { ...writable, upload: false, delete: false }
    vi.spyOn(api, 'listFiles').mockResolvedValue({
      ...response(), current_path: '/folder/', entries: [entry], next_cursor: 'next',
      is_admin: true, can_write: true, capabilities: permissions,
      max_upload_bytes: 100, max_upload_batch_bytes: 200, max_upload_batch_entries: 5,
      max_archive_bytes: 300, max_archive_entries: 10,
    })
    const state = listing()
    await state.refresh()
    expect(state.path.value).toBe('folder')
    expect(state.entries.value).toEqual([entry])
    expect(state.nextCursor.value).toBe('next')
    expect(state.isAdministrator.value).toBe(true)
    expect(state.capabilities.value).toEqual(permissions)
    expect(state.maxUploadBytes.value).toBe(100)
    expect(state.maxUploadBatchBytes.value).toBe(200)
    expect(state.maxUploadBatchEntries.value).toBe(5)
    expect(state.maxArchiveBytes.value).toBe(300)
    expect(state.maxArchiveEntries.value).toBe(10)
  })

  it.each([false, true])('retains legacy permission and batch defaults for can_write=%s', async canWrite => {
    vi.spyOn(api, 'listFiles').mockResolvedValue({ ...response(), can_write: canWrite })
    const state = listing()
    await state.refresh()
    expect(state.capabilities.value).toEqual({
      download: true, upload: canWrite, create_directory: canWrite,
      rename: canWrite, move_items: canWrite, copy: canWrite, delete: canWrite,
    })
    expect(state.maxUploadBatchBytes.value).toBe(state.maxUploadBytes.value)
    expect(state.maxUploadBatchEntries.value).toBe(1)
  })

  it('clears old entries and permissions before a storage switch response arrives', async () => {
    const pending = deferred<FileListResponse>()
    vi.spyOn(api, 'listFiles').mockResolvedValueOnce({ ...response(), entries: [entry], capabilities: writable })
      .mockReturnValueOnce(pending.promise)
    const state = listing()
    await state.refresh()
    const switching = state.switchStorage('second')
    expect(state.loading.value).toBe(true)
    expect(state.entries.value).toEqual([])
    expect(Object.values(state.capabilities.value)).toEqual(Array(7).fill(false))
    pending.resolve(response('second'))
    await switching
    expect(state.currentStorageId.value).toBe('second')
    expect(state.loading.value).toBe(false)
  })

  it('ignores a stale response before applying its preference scope or permissions', async () => {
    const pending = deferred<FileListResponse>()
    const request = vi.spyOn(api, 'listFiles').mockResolvedValueOnce(response())
      .mockReturnValueOnce(pending.promise).mockResolvedValueOnce(response('second'))
    const state = listing()
    await state.refresh()
    const old = state.refresh()
    await state.switchStorage('second')
    localStorage.setItem('ycloud:storage:user:alice', 'missing')
    pending.resolve({ ...response('first', 'user:alice'), entries: [entry], capabilities: writable })
    await old
    expect(request).toHaveBeenCalledTimes(3)
    expect(state.currentStorageId.value).toBe('second')
    expect(state.entries.value).toEqual([])
    expect(state.capabilities.value.upload).toBe(false)
    expect(localStorage.getItem('ycloud:storage:guest')).toBe('second')
    expect(localStorage.getItem('ycloud:storage:user:alice')).toBe('missing')
  })

  it('clears entries and permissions when access is revoked even if storage fallback fails', async () => {
    vi.spyOn(api, 'listFiles').mockResolvedValueOnce({ ...response(), entries: [entry], capabilities: writable })
      .mockRejectedValueOnce(new ApiError('denied', 403))
    vi.spyOn(api, 'listStorages').mockRejectedValue(new Error('offline'))
    const state = listing()
    await state.refresh()
    await state.refresh()
    expect(state.entries.value).toEqual([])
    expect(Object.values(state.capabilities.value)).toEqual(Array(7).fill(false))
    expect(state.loading.value).toBe(false)
  })

  it('clears the old identity before waiting for storages after sign-in', async () => {
    const pending = deferred<BrowserStorage[]>()
    vi.spyOn(api, 'listFiles').mockResolvedValueOnce({ ...response(), entries: [entry], capabilities: writable, is_admin: true })
      .mockResolvedValueOnce(response('second', 'user:alice'))
    vi.spyOn(api, 'listStorages').mockReturnValueOnce(pending.promise)
    const state = listing()
    await state.refresh()
    const signingIn = state.resetAfterSignIn('second')
    expect(state.entries.value).toEqual([])
    expect(Object.values(state.capabilities.value)).toEqual(Array(7).fill(false))
    expect(state.isAdministrator.value).toBe(false)
    pending.resolve(storages)
    await signingIn
    expect(state.currentStorageId.value).toBe('second')
    expect(state.loading.value).toBe(false)
  })

  it('continues loading and switching when browser preference access is blocked', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked') })
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked') })
    vi.spyOn(api, 'listFiles').mockImplementation(async (_path, id) => response(id))
    const state = listing()
    await state.refresh()
    await state.switchStorage('second')
    expect(state.currentStorageId.value).toBe('second')
    expect(state.loading.value).toBe(false)
  })
})

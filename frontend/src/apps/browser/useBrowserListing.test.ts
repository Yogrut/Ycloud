import { afterEach, describe, expect, it, vi } from 'vitest'
import * as api from '../../shared/api/browser'
import type { BrowserCapabilities, BrowserStorage, FileEntry, FileListResponse } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useBrowserListing } from './useBrowserListing'

const storages: BrowserStorage[] = ['first', 'second'].map(id => ({ id, name: id, enabled: true, ready: true, requires_login: false }))
function response(id = 'first', scope = 'guest', items = storages): FileListResponse {
  return { storage_id: id, selection_scope: scope, storages: items, current_path: '', parent_path: null, entries: [], page_start: 0, page_size: 20, next_cursor: null, can_write: false, max_upload_bytes: 1, max_archive_bytes: 1, max_archive_entries: 1 }
}
function listing() { return useBrowserListing({ announce: vi.fn(), resetSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin: vi.fn() }) }
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(yes => { resolve = yes })
  return { promise, resolve }
}

const writable: BrowserCapabilities = { download: true, upload: true, create_directory: true, rename: true, move_items: true, copy: true, delete: true }
const entry: FileEntry = { name: 'one.txt', path: 'folder/one.txt', is_dir: false, size: 3, modified: '2026-09-30T00:00:00Z', mime: 'text/plain', icon: 'file', locked: false }
afterEach(() => { vi.restoreAllMocks(); localStorage.clear() })

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

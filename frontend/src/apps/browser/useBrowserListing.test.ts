import { afterEach, describe, expect, it, vi } from 'vitest'
import * as api from '../../shared/api/browser'
import type { BrowserStorage, FileListResponse } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useBrowserListing } from './useBrowserListing'

const storages: BrowserStorage[] = ['first', 'second'].map(id => ({ id, name: id, enabled: true, ready: true, requires_login: false }))
function response(id = 'first', scope = 'guest', items = storages): FileListResponse {
  return { storage_id: id, selection_scope: scope, storages: items, current_path: '', parent_path: null, entries: [], page_start: 0, page_size: 20, next_cursor: null, can_write: false, max_upload_bytes: 1, max_archive_bytes: 1, max_archive_entries: 1 }
}
function listing() { return useBrowserListing({ announce: vi.fn(), resetSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin: vi.fn() }) }
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

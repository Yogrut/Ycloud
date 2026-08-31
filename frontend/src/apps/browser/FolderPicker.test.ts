import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import FolderPicker from './FolderPicker.vue'

afterEach(() => document.body.replaceChildren())

describe('FolderPicker', () => {
  it('loads every destination page from the frozen source storage', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({
        storage_id: 'archive',
        storages: [],
        current_path: '',
        parent_path: null,
        entries: [],
        page_start: 0,
        page_size: 100,
        next_cursor: 'next-page',
        can_write: true,
        max_upload_bytes: 0,
        max_archive_bytes: 0,
        max_archive_entries: 0,
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        storage_id: 'archive',
        storages: [],
        current_path: '',
        parent_path: null,
        entries: [],
        page_start: 0,
        page_size: 100,
        next_cursor: null,
        can_write: true,
        max_upload_bytes: 0,
        max_archive_bytes: 0,
        max_archive_entries: 0,
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)

    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(FolderPicker, { title: 'Move to', storageId: 'archive' })
    app.mount(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(fetchMock).toHaveBeenCalledTimes(2)
    for (const [input] of fetchMock.mock.calls) {
      const url = new URL(String(input), 'http://localhost')
      expect(url.searchParams.get('storage_id')).toBe('archive')
    }
    expect(new URL(String(fetchMock.mock.calls[1]?.[0]), 'http://localhost').searchParams.get('cursor')).toBe('next-page')
    app.unmount()
  })
})

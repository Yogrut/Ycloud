import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import FolderPicker from './FolderPicker.vue'

afterEach(() => document.body.replaceChildren())

function mountPicker(host: HTMLElement, onConfirm?: (path: string) => void) {
  const app = createApp(FolderPicker, { title: 'Move to', storageId: 'archive', onConfirm })
  app.mount(host)
  return app
}

describe('FolderPicker', () => {
  it('keeps the latest destination when directory responses arrive out of order', async () => {
    const response = (path: string, entries: object[] = []) => new Response(JSON.stringify({
      storage_id: 'archive', storages: [], current_path: path, parent_path: null,
      entries, page_start: 0, page_size: 100, next_cursor: null, can_write: true,
      max_upload_bytes: 0, max_archive_bytes: 0, max_archive_entries: 0,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })
    let resolveFirst!: (value: Response) => void
    let resolveSecond!: (value: Response) => void
    const first = new Promise<Response>(resolve => { resolveFirst = resolve })
    const second = new Promise<Response>(resolve => { resolveSecond = resolve })
    vi.stubGlobal('fetch', vi.fn((input: string) => {
      const path = new URL(String(input), 'http://localhost').searchParams.get('path')
      if (path === '/first') return first
      if (path === '/second') return second
      return Promise.resolve(response('', [
        { name: 'first', path: 'first', is_dir: true, locked: false },
        { name: 'second', path: 'second', is_dir: true, locked: false },
      ]))
    }))

    const host = document.createElement('div')
    document.body.append(host)
    const chosen = vi.fn()
    const app = mountPicker(host, chosen)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    const rows = host.querySelectorAll<HTMLButtonElement>('.picker-row')
    expect(rows).toHaveLength(2)
    rows[0]?.click()
    rows[1]?.click()
    resolveSecond(response('second'))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    resolveFirst(response('first'))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    expect(host.querySelector('.picker-path')?.textContent).toBe('/second')
    host.querySelector<HTMLButtonElement>('.modal-actions button:last-child')?.click()
    expect(chosen).toHaveBeenCalledWith('second')
    app.unmount()
  })

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
    const app = mountPicker(host)
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

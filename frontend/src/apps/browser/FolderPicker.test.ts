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
  it('shows the first page while a later page is still loading', async () => {
    let resolveNext!: (value: Response) => void
    const nextPage = new Promise<Response>(resolve => { resolveNext = resolve })
    const reply = (entries: object[], cursor: string | null) => new Response(JSON.stringify({
      current_path: '', entries, next_cursor: cursor,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })
    vi.stubGlobal('fetch', vi.fn()
      .mockResolvedValueOnce(reply([{ name: 'visible', path: 'visible', is_dir: true, locked: false }], 'next'))
      .mockReturnValueOnce(nextPage))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPicker(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    expect(host.querySelector('.picker-row')?.textContent).toContain('visible')
    // Later pages append in the background; the dialog is never held in a
    // blocking loading state once the first page has rendered.
    expect(host.querySelector('.picker-state')).toBeNull()
    resolveNext(reply([], null))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    expect(host.querySelectorAll('.picker-row')).toHaveLength(1)
    expect(host.querySelector('.picker-state')).toBeNull()
    app.unmount()
  })
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
    expect(host.querySelector('.picker-crumbs')?.textContent).toContain('second')
    host.querySelector<HTMLButtonElement>('.picker-choices button:last-child')?.click()
    expect(chosen).toHaveBeenCalledWith('second', 'overwrite')
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

  function listing(path: string, entries: object[], capabilities: object = { create_directory: true }) {
    return new Response(JSON.stringify({
      storage_id: 'archive', storages: [], current_path: path, parent_path: null, entries,
      page_start: 0, page_size: 100, next_cursor: null, can_write: true,
      max_upload_bytes: 0, max_archive_bytes: 0, max_archive_entries: 0, capabilities,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })
  }

  async function settle() {
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
  }

  it('hides the new-folder action without create permission', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(listing('', [], { create_directory: false })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPicker(host)
    await settle()
    expect(host.querySelector('.picker-new-folder')).toBeNull()
    app.unmount()
  })

  it('creates a folder in the current directory and reloads it', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(listing('', []))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(listing('', [{ name: 'fresh', path: 'fresh', is_dir: true, locked: false }]))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPicker(host)
    await settle()
    host.querySelector<HTMLButtonElement>('.picker-new-folder')!.click()
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('.picker-create .input')!
    input.value = 'fresh'
    input.dispatchEvent(new Event('input'))
    host.querySelector<HTMLFormElement>('.picker-create')!.dispatchEvent(new Event('submit'))
    await settle()
    await settle()
    expect(fetchMock).toHaveBeenCalledTimes(3)
    expect(host.querySelector('.picker-create')).toBeNull()
    expect(host.querySelectorAll('.picker-row')).toHaveLength(1)
    // The reload targets the directory that was on screen when submitting.
    expect(new URL(String(fetchMock.mock.calls[2]?.[0]), 'http://localhost').searchParams.get('path') ?? '').toBe('')
    app.unmount()
  })

  it('keeps the dialog open and explains a duplicate name', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(listing('', []))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: '目标路径已经存在', code: 'conflict' }), {
        status: 409, headers: { 'Content-Type': 'application/json' },
      }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPicker(host)
    await settle()
    host.querySelector<HTMLButtonElement>('.picker-new-folder')!.click()
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('.picker-create .input')!
    input.value = 'taken'
    input.dispatchEvent(new Event('input'))
    host.querySelector<HTMLFormElement>('.picker-create')!.dispatchEvent(new Event('submit'))
    await settle()
    expect(host.querySelector('.picker-create-error')?.textContent).toContain('已存在')
    expect(host.querySelector('.picker-create')).not.toBeNull()
    expect(fetchMock).toHaveBeenCalledTimes(2)
    app.unmount()
  })
})

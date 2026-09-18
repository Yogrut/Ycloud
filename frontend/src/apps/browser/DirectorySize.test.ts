import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'
import { formatSize } from '../../shared/format'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })
const entry = (name: string, folder = true) => ({ name, path: name, is_dir: folder, size: 23, modified: '', mime: '', icon: folder ? 'folder' : 'file', locked: false })
const listing = { storage_id: 'primary', storages: [], current_path: '', parent_path: null,
  entries: [entry('notes'), entry('readme.txt', false)], page_start: 1, page_size: 20, next_cursor: null,
  can_write: false, max_upload_bytes: 100, max_archive_bytes: 100, max_archive_entries: 100 }
const json = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'Content-Type': 'application/json' } })
async function settle() { await new Promise(resolve => setTimeout(resolve, 0)); await nextTick() }
async function mount(sizeResponse: (init?: RequestInit) => Promise<Response>) {
  const size = vi.fn(sizeResponse)
  const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input).startsWith('/api/directory/size')) return size(init)
    if (input === '/api/me') return Promise.resolve(json({ logged_in: false, is_admin: false }))
    return Promise.resolve(json(listing))
  })
  vi.stubGlobal('fetch', fetch)
  const host = document.createElement('div'); document.body.append(host)
  const app = createApp(BrowserView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } })
  app.mount(host); await settle()
  return { host, app, fetch, size, button: () => host.querySelector<HTMLButtonElement>('.directory-size-button')! }
}

describe('on-demand directory size', () => {
  it('does not scan on listing; only a click calculates, including a real zero-byte result', async () => {
    let finish!: (response: Response) => void
    const view = await mount(() => new Promise(resolve => { finish = resolve }))
    try {
      expect(view.size).not.toHaveBeenCalled()
      expect(view.button().textContent).toContain('计算')
      expect(view.host.querySelector('[data-entry-path="readme.txt"]')?.textContent).toContain('23 B')
      expect(view.host.querySelector('.file-icon.folder [data-weight="fill"]')).not.toBeNull()
      expect(view.host.querySelector('.file-icon.file [data-icon="file-text"]')).not.toBeNull()
      view.button().click(); await nextTick()
      expect(view.button().disabled).toBe(true)
      expect(view.button().textContent).toContain('计算中')
      view.button().click()
      expect(view.size).toHaveBeenCalledTimes(1)
      expect(view.host.querySelector('.file-row.selected')).toBeNull()
      finish(json({ size: 0 })); await settle()
      expect(view.button().textContent?.trim()).toBe('0 B')
      expect(view.button().disabled).toBe(false)
    } finally { view.app.unmount() }
  })
  it('uses the shared error feedback and allows retry without displaying zero', async () => {
    const view = await mount(async () => json({ message: '无法完成目录统计' }, 503))
    try {
      view.button().click(); await settle()
      expect(document.body.textContent).toContain('无法完成目录统计')
      expect(view.button().textContent?.trim()).toBe('计算')
      expect(view.button().disabled).toBe(false)
      view.size.mockResolvedValueOnce(json({ size: 1024 }))
      view.button().click(); await settle()
      expect(view.button().textContent?.trim()).toBe(formatSize(1024))
    } finally { view.app.unmount() }
  })
  it('cancels on refresh and ignores late results from the old directory view', async () => {
    let finish!: (response: Response) => void
    let signal: AbortSignal | undefined
    const view = await mount(init => { signal = init?.signal as AbortSignal; return new Promise(resolve => { finish = resolve }) })
    try {
      view.button().click(); await nextTick()
      view.host.querySelector<HTMLButtonElement>('.home-crumb')!.click(); await settle()
      expect(signal?.aborted).toBe(true)
      finish(json({ size: 9000 })); await settle()
      expect(view.button().textContent?.trim()).toBe('计算')
    } finally { view.app.unmount() }
  })
})

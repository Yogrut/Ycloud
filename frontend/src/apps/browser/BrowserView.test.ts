import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountBrowser(host: HTMLElement) {
  const app = createApp(BrowserView, {
    theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
  })
  app.mount(host)
  return app
}

describe('BrowserView', () => {
  it('renders a directory response without trusting HTML from file names', async () => {
    const response = {
      current_path: '',
      parent_path: null,
      entries: [{
        name: '<img src=x onerror=alert(1)>.txt',
        path: '<img src=x onerror=alert(1)>.txt',
        is_dir: false,
        size: 12,
        modified: '2026-08-20 12:00',
        mime: 'text/plain',
        icon: 'code',
        locked: false,
      }],
      truncated: false,
      can_write: false,
      max_upload_bytes: 1024,
      max_archive_bytes: 1024,
      max_archive_entries: 100,
    }
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(response), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('<img src=x onerror=alert(1)>.txt')
    expect(host.textContent).toContain('12 B')
    expect(host.querySelector('img')).toBeNull()
    app.unmount()
  })

  it('opens the mobile action panel when the header selects all entries', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      current_path: '',
      parent_path: null,
      entries: [{
        name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false,
      }],
      truncated: false,
      can_write: true,
      max_upload_bytes: 1024,
      max_archive_bytes: 1024,
      max_archive_entries: 100,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const selectAll = host.querySelector('.file-head .select-box') as HTMLButtonElement
    selectAll.click()
    await nextTick()

    expect(host.querySelector('.context-menu')).not.toBeNull()
    expect(host.textContent).toContain('已选择 1 项')
    expect(host.textContent).toContain('下载')
    app.unmount()
  })
})

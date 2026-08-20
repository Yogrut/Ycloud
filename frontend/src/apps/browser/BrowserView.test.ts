import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

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
    const app = createApp(BrowserView, {
      theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
    })
    app.mount(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('<img src=x onerror=alert(1)>.txt')
    expect(host.textContent).toContain('12 B')
    expect(host.querySelector('img')).toBeNull()
    app.unmount()
  })
})

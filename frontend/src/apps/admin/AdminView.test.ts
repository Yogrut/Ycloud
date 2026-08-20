import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminView from './AdminView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountAdmin(host: HTMLElement) {
  const app = createApp(AdminView, {
    theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
  })
  app.mount(host)
  return app
}

describe('AdminView', () => {
  it('shows the administrator login gate when the session is absent', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { message: 'Unauthorized' },
    }), { status: 401, headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('管理员登录')
    expect(host.querySelector('.admin-shell')).toBeNull()
    app.unmount()
  })

  it('keeps every administration section reachable during phased migration', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      username: 'admin', has_global_web_password: true, shares: [], folder_locks: [], login_security: [],
      max_upload_bytes: 1024, max_archive_bytes: 1024, max_archive_entries: 100,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    const links = [...host.querySelectorAll<HTMLAnchorElement>('.admin-nav-item')]
    expect(links.map(link => link.getAttribute('href'))).toEqual([
      '/admin#webdav', '/admin#locks', '/admin#limits', '/v2/admin/account', '/admin#security',
    ])
    app.unmount()
  })
})

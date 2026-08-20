import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminView from './AdminView.vue'

const adminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [], login_security: [],
  max_upload_bytes: 1024, max_archive_bytes: 1024, max_archive_entries: 100,
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
  window.history.replaceState(null, '', '/')
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
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    const links = [...host.querySelectorAll<HTMLAnchorElement>('.admin-nav-item')]
    expect(links.map(link => link.getAttribute('href'))).toEqual([
      '/admin?return=v2#webdav', '/v2/admin/locks', '/v2/admin/limits', '/v2/admin/account', '/v2/admin/security',
    ])
    app.unmount()
  })

  it('renders the Vue transfer limits page on its candidate route', async () => {
    window.history.replaceState(null, '', '/v2/admin/limits')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#limits-title')?.textContent).toBe('传输限制')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('传输限制')
    app.unmount()
  })

  it('renders the Vue folder locks page on its candidate route', async () => {
    window.history.replaceState(null, '', '/v2/admin/locks')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#locks-title')?.textContent).toBe('网页文件夹锁')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('文件夹锁')
    app.unmount()
  })

  it('renders the Vue login security page on its candidate route', async () => {
    window.history.replaceState(null, '', '/v2/admin/security')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#security-title')?.textContent).toBe('登录安全')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('登录安全')
    app.unmount()
  })
})

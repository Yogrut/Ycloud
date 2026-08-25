import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminView from './AdminView.vue'

const adminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [],
  max_upload_bytes: 1024, max_archive_bytes: 1024, max_archive_entries: 100,
  upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: 0,
  admin_login_failures: 3, web_login_failures: 5,
  admin_login_block_seconds: 3600, web_login_block_seconds: 3600,
  security_log_retention_days: 7, security_log_max_entries: 5000,
  storage_instances: [{ id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,
  default_storage_id: 'primary',
  local_storage_path: './storage',
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
      '/v2/admin/storage', '/v2/admin/webdav', '/v2/admin/locks', '/v2/admin/limits', '/v2/admin/account', '/v2/admin/users', '/v2/admin/security',
    ])
    app.unmount()
  })

  it('renders the guarded storage setup page without exposing credentials', async () => {
    window.history.replaceState(null, '', '/v2/admin/storage')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#storage-title')?.textContent).toBe('存储设置')
    expect(host.textContent).toContain('每个存储拥有独立命名空间')
    expect(host.querySelector<HTMLInputElement>('.storage-form input')?.value).toBe('./storage')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('存储设置')
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

  it('renders the Vue WebDAV page on its candidate route', async () => {
    window.history.replaceState(null, '', '/v2/admin/webdav')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#webdav-title')?.textContent).toBe('WebDAV 挂载')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('WebDAV')
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

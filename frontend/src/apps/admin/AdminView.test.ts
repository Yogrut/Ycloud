import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminView from './AdminView.vue'

vi.mock('./DashboardView.vue', () => ({ default: { template: '<section><h1 id="dashboard-title">仪表盘</h1></section>' } }))

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
    expect(host.querySelector('.admin-login-brand')).not.toBeNull()
    expect(host.querySelector('.admin-login-form-pane')).not.toBeNull()
    expect(host.querySelector('.admin-login-brand')?.textContent).toContain('统一入口，管理所有存储')
    expect(host.querySelector('.admin-login-platform')?.textContent).toContain('Ycloud')
    expect(host.querySelector('.admin-login-hub')?.textContent).toContain('权限控制')
    expect(host.querySelectorAll('.admin-login-node')).toHaveLength(2)
    expect(host.querySelector('.admin-login-storage-row')?.textContent).toContain('本地存储')
    expect(host.querySelector('.admin-login-storage-row')?.textContent).toContain('S3 存储')
    expect(host.querySelector('.admin-login-storage-row')?.textContent).not.toContain('WebDAV')
    expect(host.querySelector('.admin-login-card .locale-toggle')).toBeNull()
    expect(host.textContent).not.toContain('协议')
    expect(host.querySelector('.admin-shell')).toBeNull()
    app.unmount()
  })

  it('reveals the authenticator field only after the server requests it', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Unauthorized' } }), {
        status: 401, headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        success: false, is_admin: true, totp_required: true, message: '请输入动态验证码或恢复码',
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('input[autocomplete="one-time-code"]')).toBeNull()
    const username = host.querySelector('input[autocomplete="username"]') as HTMLInputElement
    const password = host.querySelector('input[autocomplete="current-password"]') as HTMLInputElement
    username.value = 'admin'
    username.dispatchEvent(new Event('input'))
    password.value = 'correct-password'
    password.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('input[autocomplete="one-time-code"]')).not.toBeNull()
    expect(host.textContent).toContain('验证并登录')
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
      '/admin/dashboard', '/admin/storage', '/admin/webdav', '/admin/locks', '/admin/limits', '/admin/account', '/admin/users', '/admin/protection', '/admin/security',
    ])
    expect(host.querySelector('#dashboard-title')?.textContent).toBe('仪表盘')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('仪表盘')
    expect(host.querySelector('.admin-nav-heading')).toBeNull()
    expect(host.querySelector('.admin-header .admin-nav-brand')?.textContent).toContain('Ycloud 管理')
    expect(host.querySelector('.admin-header .top-actions')).not.toBeNull()
    expect(host.querySelector('.admin-nav .admin-nav-brand')).toBeNull()
    expect(host.querySelector('.admin-floating-actions')).toBeNull()
    expect(host.querySelector('.topbar')).toBeNull()
    app.unmount()
  })

  it('renders the guarded storage setup page without exposing credentials', async () => {
    window.history.replaceState(null, '', '/admin/storage')
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
    expect(host.querySelector('.storage-form')).toBeNull()
    expect(host.querySelector('.storage-source-row')?.textContent).toContain('存储用量')
    expect(host.querySelector('.storage-source-row')?.textContent).not.toContain('./storage')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('存储设置')
    app.unmount()
  })

  it('renders the Vue transfer limits page on its candidate route', async () => {
    window.history.replaceState(null, '', '/admin/limits')
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
    window.history.replaceState(null, '', '/admin/webdav')
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
    window.history.replaceState(null, '', '/admin/locks')
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

  it('renders the Vue access logs page on its candidate route', async () => {
    window.history.replaceState(null, '', '/admin/security')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#security-title')?.textContent).toBe('访问日志')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('访问日志')
    app.unmount()
  })

  it('renders sign-in protection on its own route', async () => {
    window.history.replaceState(null, '', '/admin/protection')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#protection-title')?.textContent).toBe('登录保护')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('登录保护')
    app.unmount()
  })
})

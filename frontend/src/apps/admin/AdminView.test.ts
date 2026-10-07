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
  storage_instances: [{ id: 'primary', name: '本地存储', enabled: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,

  local_storage_path: './storage',
}

afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.replaceChildren()
  window.history.replaceState(null, '', '/')
})

async function mountAdmin(host: HTMLElement) {
  const app = createApp(AdminView, {
    theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
  })
  app.mount(host)
  await vi.dynamicImportSettled()
  await nextTick()
  return app
}

describe('AdminView', () => {
  it('switches sections without remounting the shell and revalidates configuration in the background', async () => {
    window.history.replaceState(null, '', '/admin/dashboard')
    let finishRead!: (response: Response) => void
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } }))
      .mockImplementationOnce(() => new Promise<Response>(resolve => { finishRead = resolve }))
      .mockResolvedValueOnce(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      const shell = host.querySelector('.admin-shell')
      const link = host.querySelector<HTMLAnchorElement>('a[href="/admin/webdav"]')!
      const click = new MouseEvent('click', { bubbles: true, cancelable: true })
      link.dispatchEvent(click)
      await nextTick()
      expect(click.defaultPrevented).toBe(true)
      expect(window.location.pathname).toBe('/admin/webdav')
      expect(host.querySelector('.admin-shell')).toBe(shell)
      expect(host.querySelector('.admin-loading')).toBeNull()
      expect(host.querySelector('.admin-nav-item.active')?.getAttribute('href')).toBe('/admin/webdav')
      expect(fetchMock).toHaveBeenCalledTimes(2)
      finishRead(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      window.history.replaceState(null, '', '/admin/dashboard')
      window.dispatchEvent(new PopStateEvent('popstate'))
      await nextTick()
      expect(host.querySelector('#dashboard-title')).not.toBeNull()
      expect(host.querySelector('.admin-shell')).toBe(shell)
    } finally { app.unmount() }
    const calls = fetchMock.mock.calls.length
    window.dispatchEvent(new PopStateEvent('popstate'))
    expect(fetchMock).toHaveBeenCalledTimes(calls)
  })

  it('preserves modified navigation clicks for opening a separate tab', async () => {
    window.history.replaceState(null, '', '/admin/dashboard')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      const push = vi.spyOn(window.history, 'pushState')
      const event = new MouseEvent('click', { bubbles: true, cancelable: true, ctrlKey: true })
      host.querySelector('a[href="/admin/webdav"]')!.dispatchEvent(event)
      expect(event.defaultPrevented).toBe(false)
      expect(push).not.toHaveBeenCalled()
    } finally { app.unmount() }
  })
  it.each(['/admin/limits', '/admin/limits/'])('uses the normalized navigation route %s', async path => {
    window.history.replaceState(null, '', path)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      await nextTick()
      expect(host.querySelector('#limits-title')?.textContent).toBe('传输限制')
      expect(host.querySelector<HTMLAnchorElement>('.admin-nav-item.active')?.getAttribute('href')).toBe('/admin/limits')
      expect(host.querySelector('.admin-nav-item.active')?.getAttribute('aria-current')).toBe('page')
    } finally { app.unmount() }
  })

  it('keeps the settings page mounted during a confirmed save refresh and reports a read failure separately', async () => {
    window.history.replaceState(null, '', '/admin/limits')
    let finishRead!: (response: Response) => void
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        settings: {
          total: { enabled: false, upload: 0, download: 0 }, guest: { enabled: false, upload: 0, download: 0 },
          users_total: { enabled: false, upload: 0, download: 0 }, users: {},
          cycle: { unit: 'months', every: 1, anchor: 0, offset_minutes: 0 },
        },
        total: { upload: 0, download: 0 }, guest: { upload: 0, download: 0 }, users_total: { upload: 0, download: 0 },
        users: {}, days: {}, next_reset: 0, today: '2026-10-02',
      }), { headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
      .mockImplementationOnce(() => new Promise<Response>(resolve => { finishRead = resolve }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      await nextTick()
      const page = host.querySelector('.limits-pane')
      expect(page).not.toBeNull()
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[4]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.settings-drawer input')!
      input.value = '1'
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(4)
      expect(fetchMock.mock.calls[2]![0]).toBe('/api/admin/limits')
      expect(JSON.parse(fetchMock.mock.calls[2]![1].body)).toMatchObject({ upload_rate_bytes_per_sec: 1024 ** 2 })
      expect(host.querySelector('.limits-pane')).toBe(page)
      expect(host.querySelector('#limits-title')?.textContent).toBe('传输限制')
      expect(host.querySelector('.admin-loading')).toBeNull()
      expect(document.querySelector('.app-toast.success')?.textContent).toContain('已保存')
      finishRead(new Response(JSON.stringify({ error: { message: 'Refresh unavailable' } }), { status: 500, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      await nextTick()
      expect(host.querySelector('.limits-pane')).toBe(page)
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Refresh unavailable')
      expect(fetchMock).toHaveBeenCalledTimes(4)
    } finally { app.unmount() }
  })

  it('keeps a submitted login running after unmount without requesting administrator information', async () => {
    let finishLogin!: (response: Response) => void
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Unauthorized' } }), { status: 401, headers: { 'Content-Type': 'application/json' } }))
      .mockImplementationOnce(() => new Promise<Response>(resolve => { finishLogin = resolve }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    let mounted = true
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      await nextTick()
      const username = host.querySelector<HTMLInputElement>('input[autocomplete="username"]')!
      username.value = ' admin '
      username.dispatchEvent(new Event('input'))
      const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = '  current-password  '
      password.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(JSON.parse(fetchMock.mock.calls[1]![1].body)).toEqual({ username: 'admin', password: '  current-password  ' })
      const signal = fetchMock.mock.calls[1]![1].signal as AbortSignal
      app.unmount()
      mounted = false
      expect(signal.aborted).toBe(false)
      finishLogin(new Response(JSON.stringify({ success: true, is_admin: true }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(signal.aborted).toBe(false)
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(document.querySelector('.app-toast')).toBeNull()
    } finally { if (mounted) app.unmount() }
  })

  it('shows a background configuration read error while retaining the storage page', async () => {
    vi.useFakeTimers()
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false)
    window.history.replaceState(null, '', '/admin/storage')
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(adminInfo), { headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Configuration unavailable' } }), { status: 500, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
    try {
      await vi.advanceTimersByTimeAsync(0)
      await nextTick()
      await vi.advanceTimersByTimeAsync(300_000)
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(host.querySelector('#storage-title')?.textContent).toBe('存储设置')
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Configuration unavailable')
      expect(host.querySelector('.admin-login-shell')).toBeNull()
    } finally { app.unmount() }
  })

  it('shows the administrator login gate when the session is absent', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { message: 'Unauthorized' },
    }), { status: 401, headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
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
    const app = await mountAdmin(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#protection-title')?.textContent).toBe('登录保护')
    expect(host.querySelector('.admin-nav-item.active')?.textContent).toContain('登录保护')
    app.unmount()
  })
})

import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo, LoginEvent } from '../../shared/api/admin'
import SecurityView from './SecurityView.vue'

const now = Math.floor(Date.now() / 1000)
const info: AdminInfo = {
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
const normal: LoginEvent = {
  id: 2, entry: 'admin', success: true, ip: '192.0.2.10', occurred_at: now,
  result: '登录成功', failed_attempts: 0, blocked_until: null,
  user_agent: 'Browser A', current_blocked_until: null,
}
const failed: LoginEvent = {
  id: 1, entry: 'web', success: false, ip: '192.0.2.11', occurred_at: now,
  result: '凭据错误', failed_attempts: 2, blocked_until: null,
  user_agent: 'Browser B', current_blocked_until: null,
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function eventResponse(url: string): Response {
  const success = new URL(url, 'http://localhost').searchParams.get('success')
  const events = success === 'false' ? [failed] : success === 'true' ? [normal] : [normal, failed]
  return new Response(JSON.stringify({ events, total: events.length, page: 1, next_cursor: null }), {
    status: 200, headers: { 'Content-Type': 'application/json' },
  })
}

function mountSecurity(host: HTMLElement, onChanged?: (message: string) => void) {
  const app = createApp(SecurityView, { info, onChanged })
  app.mount(host)
  return app
}

describe('SecurityView', () => {
  it('retains rejected retention edits for a manual retry without reporting success', async () => {
    let denied = true
    const fetchMock = vi.fn((url: string, options?: RequestInit) => {
      if (options?.method === 'PUT') return Promise.resolve(denied
        ? new Response(JSON.stringify({ error: { code: 'access_denied', message: 'Retention denied' } }), { status: 403, headers: { 'Content-Type': 'application/json' } })
        : new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
      return Promise.resolve(eventResponse(url))
    })
    vi.stubGlobal('fetch', fetchMock)
    const changed = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host, changed)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      host.querySelector<HTMLButtonElement>('button[aria-label="日志设置"]')!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.drawer-form input')!
      input.value = '1000'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('.drawer-form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(changed).not.toHaveBeenCalled()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Retention denied')
      expect(host.querySelector('.settings-drawer')).not.toBeNull()
      expect(input.value).toBe('1000')
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
      expect(fetchMock.mock.calls.filter(([, options]) => options?.method === 'PUT')).toHaveLength(1)
      denied = false
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      const writes = fetchMock.mock.calls.filter(([, options]) => options?.method === 'PUT')
      expect(writes).toHaveLength(2)
      for (const [, options] of writes) expect(JSON.parse(String(options?.body))).toEqual({ security_log_retention_days: 7, security_log_max_entries: 1000 })
      expect(changed).toHaveBeenCalledTimes(1)
    } finally { app.unmount() }
  })

  it('reports a confirmed restriction once before a pending log refresh finishes', async () => {
    let finishWrite!: (response: Response) => void
    let finishRead!: (response: Response) => void
    let reads = 0
    const fetchMock = vi.fn((url: string, options?: RequestInit) => {
      if (options?.method === 'POST') return new Promise<Response>(resolve => { finishWrite = resolve })
      if (++reads === 1) return Promise.resolve(eventResponse(url))
      return new Promise<Response>(resolve => { finishRead = resolve })
    })
    vi.stubGlobal('fetch', fetchMock)
    const changed = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host, changed)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      host.querySelector<HTMLButtonElement>('.security-operation button')!.click()
      await nextTick()
      const confirm = host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!
      confirm.click()
      confirm.click()
      expect(fetchMock.mock.calls.filter(([, options]) => options?.method === 'POST')).toHaveLength(1)
      expect(changed).not.toHaveBeenCalled()
      finishWrite(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(changed).toHaveBeenCalledWith('已限制 192.0.2.10 的管理员登录')
      expect(host.querySelector('.confirmation-actions')).toBeNull()
      finishRead(new Response(JSON.stringify({ error: { code: 'unavailable', message: 'Refresh unavailable' } }), { status: 503, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(changed).toHaveBeenCalledTimes(1)
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Refresh unavailable')
    } finally { app.unmount() }
  })

  it.each(['PUT', 'POST', 'DELETE'])('does not cancel a submitted %s write or launch a refresh after unmounting', async method => {
    let finish!: (response: Response) => void
    let writeSignal: AbortSignal | undefined
    const fetchMock = vi.fn((url: string, options?: RequestInit) => {
      if (options?.method === method) {
        writeSignal = options.signal ?? undefined
        return new Promise<Response>(resolve => { finish = resolve })
      }
      return Promise.resolve(eventResponse(url))
    })
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    let mounted = true
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      if (method === 'PUT') {
        host.querySelector<HTMLButtonElement>('button[aria-label="日志设置"]')!.click()
        await nextTick()
        host.querySelector('.drawer-form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      } else {
        host.querySelector<HTMLButtonElement>(method === 'POST' ? '.security-operation button' : '.log-clear')!.click()
        await nextTick()
        host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
      }
      expect(fetchMock).toHaveBeenCalledTimes(2)
      app.unmount()
      mounted = false
      expect(writeSignal?.aborted).toBe(false)
      finish(method === 'DELETE' ? new Response(null, { status: 204 }) : new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(writeSignal?.aborted).toBe(false)
    } finally { if (mounted) app.unmount() }
  })

  it('keeps a missing unblock target distinct from a successful restriction change', async () => {
    const fetchMock = vi.fn((_url: string, options?: RequestInit) => options?.method === 'POST'
      ? Promise.resolve(new Response(JSON.stringify({ error: { code: 'not_found', message: 'Not found' } }), { status: 404, headers: { 'Content-Type': 'application/json' } }))
      : Promise.resolve(new Response(JSON.stringify({ events: [{ ...normal, current_blocked_until: now + 3600 }], total: 1, page: 1, next_cursor: null }), { headers: { 'Content-Type': 'application/json' } })))
    vi.stubGlobal('fetch', fetchMock)
    const changed = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host, changed)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      host.querySelector<HTMLButtonElement>('.security-operation button')!.click()
      await nextTick()
      host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('该 IP 当前没有可解除的限制')
      expect(changed).not.toHaveBeenCalled()
      expect(fetchMock).toHaveBeenCalledTimes(2)
    } finally { app.unmount() }
  })

  it('saves retention only once and reports the confirmed write before refreshing logs', async () => {
    let finishSave!: (response: Response) => void
    let finishRefresh!: (response: Response) => void
    let reads = 0
    const fetchMock = vi.fn((url: string, options?: RequestInit) => {
      if (options?.method === 'PUT') return new Promise<Response>(resolve => { finishSave = resolve })
      if (++reads === 1) return Promise.resolve(eventResponse(url))
      return new Promise<Response>(resolve => { finishRefresh = resolve })
    })
    vi.stubGlobal('fetch', fetchMock)
    const changed = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host, changed)
    try {
      await new Promise(resolve => setTimeout(resolve, 0))
      host.querySelector<HTMLButtonElement>('button[aria-label="日志设置"]')!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.drawer-form input')!
      input.value = '1000'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('.drawer-form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      expect(fetchMock.mock.calls.filter(([, options]) => options?.method === 'PUT')).toHaveLength(1)
      finishSave(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(changed).toHaveBeenCalledWith('登录日志保存策略已更新')
      expect(host.querySelector('.settings-drawer')).toBeNull()
      finishRefresh(new Response(JSON.stringify({ error: { code: 'unavailable', message: 'Log refresh failed' } }), { status: 503, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(changed).toHaveBeenCalledTimes(1)
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Log refresh failed')
    } finally { app.unmount() }
  })

  it('confirms before clearing all logs, keeps failures visible, and resets the page on success', async () => {
    let fail = true
    const fetchMock = vi.fn((url: string, options?: RequestInit) => {
      if (options?.method === 'DELETE') return Promise.resolve(fail
        ? new Response(JSON.stringify({ error: 'clear failed' }), { status: 500, headers: { 'Content-Type': 'application/json' } })
        : new Response(null, { status: 204 }))
      return Promise.resolve(eventResponse(url))
    })
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    host.querySelector<HTMLButtonElement>('.log-clear')!.click(); await nextTick()
    expect(fetchMock.mock.calls.some(([, options]) => options?.method === 'DELETE')).toBe(false)
    expect(host.textContent).toContain('不仅是当前筛选结果')
    expect(host.querySelector('.confirmation-warning')?.textContent).toContain('删除后不可恢复')
    expect(Array.from(host.querySelectorAll('.confirmation-actions button'), button => button.textContent)).toEqual(['取消', '清空日志'])
    expect(host.querySelector('.confirmation-actions .btn.danger')).not.toBeNull()
    host.querySelector<HTMLButtonElement>('.confirmation-actions .secondary')!.click(); await nextTick()
    expect(fetchMock.mock.calls.some(([, options]) => options?.method === 'DELETE')).toBe(false)
    host.querySelector<HTMLButtonElement>('.log-clear')!.click(); await nextTick()
    host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(document.querySelector('.app-toast.error')?.textContent).toBeTruthy()
    fail = false
    host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('.modal')).toBeNull()
    expect(String(fetchMock.mock.calls.at(-1)?.[0])).toContain('page=1')
    app.unmount()
  })

  it('supports page jumping and row count selection without downloading the entire log', async () => {
    const fetchMock = vi.fn((url: string) => Promise.resolve(new Response(JSON.stringify({events: [normal], total: 143, page: Number(new URL(url, 'http://localhost').searchParams.get('page') ?? 1), next_cursor: null}), {headers:{'Content-Type':'application/json'}})))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.textContent).toContain('共 143 条')
    const jump = host.querySelector<HTMLInputElement>('.log-page-jump input')!
    jump.value = '3'; jump.dispatchEvent(new Event('input'))
    host.querySelector('form.log-page-jump')!.dispatchEvent(new Event('submit', {cancelable:true}))
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(String(fetchMock.mock.calls.at(-1)?.[0])).toContain('page=3')
    host.querySelector<HTMLButtonElement>('.log-page-size .app-select-trigger')!.click(); await nextTick()
    host.querySelectorAll<HTMLButtonElement>('.log-page-size .app-select-option')[1]!.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(String(fetchMock.mock.calls.at(-1)?.[0])).toContain('limit=50&page=1')
    const search = host.querySelector<HTMLInputElement>('.log-search input')!
    search.value = 'Browser'; search.dispatchEvent(new Event('input'))
    host.querySelector('.log-search')!.dispatchEvent(new Event('submit', {cancelable:true}))
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(String(fetchMock.mock.calls.at(-1)?.[0])).toContain('search=Browser')
    app.unmount()
  })

  it('discards unsaved log retention changes when the drawer is reopened', async () => {
    vi.stubGlobal('fetch', vi.fn((url: string) => Promise.resolve(eventResponse(url))))
    const host = document.createElement('div'); document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    host.querySelector<HTMLButtonElement>('button[aria-label="日志设置"]')!.click(); await nextTick()
    const input = host.querySelector<HTMLInputElement>('.drawer-form input')!
    input.value = '1000'; input.dispatchEvent(new Event('input'))
    host.querySelector<HTMLButtonElement>('.drawer-close')!.click(); await nextTick()
    host.querySelector<HTMLButtonElement>('button[aria-label="日志设置"]')!.click(); await nextTick()
    expect(host.querySelector<HTMLInputElement>('.drawer-form input')!.value).toBe('5000')
    app.unmount()
  })

  it('loads successful and failed events separately from the server', async () => {
    const fetchMock = vi.fn((url: string) => Promise.resolve(eventResponse(url)))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('#security-title')?.textContent).toBe('访问日志')
    expect(host.textContent).toContain('192.0.2.10')
    expect(new URL(fetchMock.mock.calls[0]![0], 'http://localhost').searchParams.get('limit')).toBe('20')
    expect(host.textContent).toContain('共 2 条')
    host.querySelector<HTMLButtonElement>('.log-status-filter .app-select-trigger')!.click()
    await nextTick()
    host.querySelectorAll<HTMLButtonElement>('.log-status-filter .app-select-option')[2]!.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(host.textContent).toContain('192.0.2.11')
    expect(host.textContent).not.toContain('192.0.2.10')
    app.unmount()
  })

  it('confirms a manual restriction before sending it to the server', async () => {
    const fetchMock = vi.fn((url: string) => {
      if (url === '/api/admin/security/block') {
        return Promise.resolve(new Response(JSON.stringify({ success: true }), {
          status: 200, headers: { 'Content-Type': 'application/json' },
        }))
      }
      return Promise.resolve(eventResponse(url))
    })
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    ;(host.querySelector('.security-operation button') as HTMLButtonElement).click()
    await nextTick()
    ;(host.querySelector('.confirmation-actions .btn:not(.secondary)') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/block', expect.objectContaining({
      body: JSON.stringify({ entry: 'admin', ip: '192.0.2.10' }),
    }))
    app.unmount()
  })
})

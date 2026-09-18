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
  storage_instances: [{ id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,
  default_storage_id: 'primary',
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

function mountSecurity(host: HTMLElement) {
  const app = createApp(SecurityView, { info })
  app.mount(host)
  return app
}

describe('SecurityView', () => {
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

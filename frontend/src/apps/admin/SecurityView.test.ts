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
  const successful = new URL(url, 'http://localhost').searchParams.get('success') === 'true'
  return new Response(JSON.stringify({ events: [successful ? normal : failed], next_cursor: null }), {
    status: 200, headers: { 'Content-Type': 'application/json' },
  })
}

function mountSecurity(host: HTMLElement) {
  const app = createApp(SecurityView, { info })
  app.mount(host)
  return app
}

describe('SecurityView', () => {
  it('loads successful and failed events separately from the server', async () => {
    vi.stubGlobal('fetch', vi.fn((url: string) => Promise.resolve(eventResponse(url))))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('192.0.2.10')
    host.querySelectorAll<HTMLButtonElement>('.security-tabs button')[1]?.click()
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
    ;(host.querySelector('.modal .btn.danger') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/block', expect.objectContaining({
      body: JSON.stringify({ entry: 'admin', ip: '192.0.2.10' }),
    }))
    app.unmount()
  })
})

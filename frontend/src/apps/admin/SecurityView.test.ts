import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { LoginRecord } from '../../shared/api/admin'
import SecurityView from './SecurityView.vue'

const now = Math.floor(Date.now() / 1000)
const records: LoginRecord[] = [
  {
    entry: 'admin', ip: '192.0.2.10', failed_attempts: 0, blocked_until: null,
    last_attempt_at: now, last_success_at: now, last_result: '登录成功', user_agent: 'Browser A',
  },
  {
    entry: 'web', ip: '192.0.2.11', failed_attempts: 2, blocked_until: null,
    last_attempt_at: now, last_success_at: null, last_result: '凭据错误', user_agent: 'Browser B',
  },
]

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountSecurity(host: HTMLElement, loginRecords = records) {
  const app = createApp(SecurityView, { records: loginRecords })
  app.mount(host)
  return app
}

describe('SecurityView', () => {
  it('separates normal records from error and restriction records', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    await nextTick()

    expect(host.textContent).toContain('192.0.2.10')
    expect(host.textContent).not.toContain('192.0.2.11')
    const tabs = host.querySelectorAll<HTMLButtonElement>('.security-tabs button')
    tabs[1]?.click()
    await nextTick()

    expect(host.textContent).toContain('192.0.2.11')
    expect(host.textContent).not.toContain('192.0.2.10')
    app.unmount()
  })

  it('confirms a manual restriction before sending it to the server', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host)
    await nextTick()

    ;(host.querySelector('.security-action') as HTMLButtonElement).click()
    await nextTick()
    expect(host.textContent).toContain('确认封禁 IP')
    ;(host.querySelector('.modal .btn.danger') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/block', expect.objectContaining({
      body: JSON.stringify({ entry: 'admin', ip: '192.0.2.10' }),
    }))
    app.unmount()
  })

  it('paginates large record sets instead of expanding the page indefinitely', async () => {
    const many = Array.from({ length: 11 }, (_, index): LoginRecord => ({
      entry: 'admin', ip: `192.0.2.${index + 1}`, failed_attempts: 0, blocked_until: null,
      last_attempt_at: now - index, last_success_at: now - index, last_result: '登录成功', user_agent: null,
    }))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountSecurity(host, many)
    await nextTick()

    expect(host.querySelectorAll('.security-record')).toHaveLength(10)
    expect(host.textContent).toContain('第 1 / 2 页')
    app.unmount()
  })
})

import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminView from './AdminView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('AdminView', () => {
  it('shows the administrator login gate when the session is absent', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { message: 'Unauthorized' },
    }), { status: 401, headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(AdminView, {
      theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
    })
    app.mount(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('管理员登录')
    expect(host.querySelector('.admin-shell')).toBeNull()
    app.unmount()
  })
})

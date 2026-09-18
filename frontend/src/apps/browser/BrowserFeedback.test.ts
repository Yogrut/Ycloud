import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })
const settle = () => new Promise(resolve => setTimeout(resolve, 0))

function mountBrowser(canWrite: boolean, fail = false) {
  const request = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'POST') return Promise.resolve(new Response(JSON.stringify(fail ? { error: { message: '没有创建目录的权限' } } : { success: true }), { status: fail ? 403 : 200, headers: { 'Content-Type': 'application/json' } }))
    if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
    return Promise.resolve(new Response(JSON.stringify({ storage_id: 'primary', storages: [], current_path: '', entries: [], can_write: canWrite, next_cursor: null })))
  })
  vi.stubGlobal('fetch', request)
  const host = document.createElement('div'); document.body.append(host)
  const app = createApp(BrowserView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } })
  app.mount(host)
  return { app, host, request }
}

describe('browser feedback', () => {
  it('uses the shared error toast for denied actions and can repeat after dismissal', async () => {
    const { app, host, request } = mountBrowser(false)
    await settle()
    const create = host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')!
    create.click(); await nextTick()
    expect(document.querySelector('.app-toast.error[role="alert"]')?.textContent).toBe('无权限')
    expect(host.querySelector('.toast')).toBeNull()
    document.querySelector<HTMLButtonElement>('.app-toast button')!.click(); await nextTick()
    expect(document.querySelector('.app-toast')).toBeNull()
    create.click(); await nextTick()
    expect(document.querySelectorAll('.app-toast.error')).toHaveLength(1)
    expect(request.mock.calls.some(([, init]) => init?.method === 'POST')).toBe(false)
    app.unmount()
    expect(document.querySelector('.app-toast')).toBeNull()
  })

  it.each([true, false])('uses shared feedback for a folder request (failure: %s)', async fail => {
    const { app, host } = mountBrowser(true, fail)
    await settle()
    host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')!.click(); await nextTick()
    const input = host.querySelector<HTMLInputElement>('.modal input')!
    input.value = 'documents'; input.dispatchEvent(new Event('input')); await nextTick()
    host.querySelector<HTMLFormElement>('.modal')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await settle()
    expect(document.querySelector(`.app-toast.${fail ? 'error' : 'success'}`)?.textContent).toContain(fail ? '没有创建目录的权限' : '文件夹已创建')
    expect(host.querySelector('.modal-error')).toBeNull()
    expect(!!host.querySelector('.modal')).toBe(fail)
    app.unmount()
  })
})

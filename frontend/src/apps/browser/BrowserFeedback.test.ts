import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })
const settle = () => new Promise(resolve => setTimeout(resolve, 0))

function mountView() {
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(BrowserView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } })
  app.mount(host)
  return { app, host }
}

function mountBrowser(canWrite: boolean, fail = false) {
  const request = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'POST') return Promise.resolve(new Response(JSON.stringify(fail ? { error: { message: '没有创建目录的权限' } } : { success: true }), { status: fail ? 403 : 200, headers: { 'Content-Type': 'application/json' } }))
    if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
    return Promise.resolve(new Response(JSON.stringify({ storage_id: 'primary', storages: [], current_path: '', entries: [], can_write: canWrite, next_cursor: null })))
  })
  vi.stubGlobal('fetch', request)
  return { ...mountView(), request }
}

describe('browser feedback', () => {
  it.each(['delete', 'move', 'copy'] as const)('does not cancel an accepted batch %s or refresh after leaving the page', async operation => {
    let resolveWrite!: (response: Response) => void
    const pendingWrite = new Promise<Response>(resolve => { resolveWrite = resolve })
    const request = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>(input => {
      if (String(input).startsWith(`/api/batch/${operation}`)) return pendingWrite
      if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
      return Promise.resolve(new Response(JSON.stringify({
        storage_id: 'primary', storages: [], current_path: '', can_write: true, next_cursor: null,
        entries: [{ name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false }],
      })))
    })
    vi.stubGlobal('fetch', request)
    const { app, host } = mountView()
    let mounted = true
    try {
      await settle()
      host.querySelector<HTMLElement>('.file-row')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
      await nextTick()
      const label = operation === 'delete' ? '删除' : operation === 'move' ? '移动' : '复制'
      const action = [...host.querySelectorAll<HTMLButtonElement>('.context-menu .menu-item')].find(button => button.textContent?.includes(label))!
      action.click()
      await settle()
      const selector = operation === 'delete' ? '.confirmation-actions .btn:not(.secondary)' : '.picker-modal .btn:not(.secondary)'
      const confirm = document.querySelector<HTMLButtonElement>(selector)!
      confirm.click()
      confirm.click()
      await nextTick()
      const writeCalls = request.mock.calls.filter(([input]) => String(input).startsWith(`/api/batch/${operation}`))
      expect(writeCalls).toHaveLength(1)
      expect(writeCalls[0]?.[1]?.method).toBe(operation === 'move' ? 'PUT' : 'POST')
      expect(JSON.parse(String(writeCalls[0]?.[1]?.body))).toEqual({ paths: ['one.txt'], target: '' })
      const signal = writeCalls[0]?.[1]?.signal as AbortSignal
      const before = request.mock.calls.length
      app.unmount()
      mounted = false
      expect(signal.aborted).toBe(false)
      resolveWrite(new Response(JSON.stringify({ success: 1, failed: 0, results: [
        { path: 'one.txt', status: 200, code: 'ok', message: 'Completed' },
      ] }), { status: 200 }))
      await settle()
      expect(request).toHaveBeenCalledTimes(before)
      expect(document.querySelector('.app-toast')).toBeNull()
    } finally {
      if (mounted) app.unmount()
      resolveWrite(new Response(JSON.stringify({ success: 1, failed: 0, results: [] }), { status: 200 }))
      await settle()
    }
  })

  it.each(['folder', 'rename'] as const)('does not repeat an unknown %s write through the open editor', async kind => {
    const request = vi.fn((input: RequestInfo | URL) => {
      if (/^\/api\/(mkdir|rename)/.test(String(input))) return Promise.resolve(new Response(JSON.stringify({
        error: { code: 'operation_result_unknown', message: 'Verify first' },
      }), { status: 503 }))
      if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
      return Promise.resolve(new Response(JSON.stringify({
        storage_id: 'primary', storages: [], current_path: '', can_write: true, next_cursor: null,
        entries: [{ name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false }],
      })))
    })
    vi.stubGlobal('fetch', request)
    const { app, host } = mountView()
    try {
      await settle()
      if (kind === 'folder') host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')!.click()
      else {
        host.querySelector<HTMLElement>('.file-row')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
        await nextTick()
        const rename = [...host.querySelectorAll<HTMLButtonElement>('.context-menu .menu-item')].find(button => button.textContent?.includes('重命名'))!
        rename.click()
      }
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.short-field-dialog input')!
      input.value = 'new-name'
      input.dispatchEvent(new Event('input'))
      await nextTick()
      const form = host.querySelector<HTMLFormElement>('.short-field-dialog')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await settle()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Verify first')
      expect(form.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true)
      expect(form.querySelector<HTMLButtonElement>('button.secondary')?.disabled).toBe(false)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await settle()
      expect(request.mock.calls.filter(([input]) => /^\/api\/(mkdir|rename)/.test(String(input)))).toHaveLength(1)
      expect(document.querySelector('.app-toast.success')).toBeNull()
    } finally {
      app.unmount()
    }
  })

  it.each(['folder', 'rename'] as const)('does not cancel an accepted %s write or refresh after leaving the page', async kind => {
    let resolveWrite!: (response: Response) => void
    const pendingWrite = new Promise<Response>(resolve => { resolveWrite = resolve })
    const request = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>(input => {
      if (/^\/api\/(mkdir|rename)/.test(String(input))) return pendingWrite
      if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
      return Promise.resolve(new Response(JSON.stringify({
        storage_id: 'primary', storages: [], current_path: '', can_write: true, next_cursor: null,
        entries: [{ name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false }],
      })))
    })
    vi.stubGlobal('fetch', request)
    const { app, host } = mountView()
    await settle()
    if (kind === 'folder') host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')!.click()
    else {
      host.querySelector<HTMLElement>('.file-row')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
      await nextTick()
      const rename = [...host.querySelectorAll<HTMLButtonElement>('.context-menu .menu-item')].find(button => button.textContent?.includes('重命名'))!
      rename.click()
    }
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('.short-field-dialog input')!
    input.value = 'new-name'
    input.dispatchEvent(new Event('input'))
    await nextTick()
    host.querySelector<HTMLFormElement>('.short-field-dialog')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()
    const writeCalls = request.mock.calls.filter(([input]) => /^\/api\/(mkdir|rename)/.test(String(input)))
    expect(writeCalls).toHaveLength(1)
    const signal = writeCalls[0]?.[1]?.signal as AbortSignal
    const before = request.mock.calls.length
    app.unmount()
    expect(signal.aborted).toBe(false)
    resolveWrite(new Response(JSON.stringify({ success: true }), { status: 200 }))
    await settle()
    expect(request).toHaveBeenCalledTimes(before)
    expect(document.querySelector('.app-toast')).toBeNull()
  })

  it.each(['folder', 'rename'] as const)('keeps the %s editor open when its backdrop is clicked during submission', async kind => {
    let resolveWrite!: (response: Response) => void
    const pendingWrite = new Promise<Response>(resolve => { resolveWrite = resolve })
    const request = vi.fn((input: RequestInfo | URL) => {
      if (/^\/api\/(mkdir|rename)/.test(String(input))) return pendingWrite
      if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
      return Promise.resolve(new Response(JSON.stringify({
        storage_id: 'primary', storages: [], current_path: '', can_write: true, next_cursor: null,
        entries: [{ name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false }],
      })))
    })
    vi.stubGlobal('fetch', request)
    const { app, host } = mountView()
    try {
      await settle()
      if (kind === 'folder') host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')!.click()
      else {
        host.querySelector<HTMLElement>('.file-row')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
        await nextTick()
        const rename = [...host.querySelectorAll<HTMLButtonElement>('.context-menu .menu-item')].find(button => button.textContent?.includes('重命名'))!
        rename.click()
      }
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.short-field-dialog input')!
      input.value = 'new-name'
      input.dispatchEvent(new Event('input'))
      await nextTick()
      const form = host.querySelector<HTMLFormElement>('.short-field-dialog')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      const overlay = form.parentElement!
      overlay.dispatchEvent(new MouseEvent('click', { bubbles: true }))
      await nextTick()
      expect(host.querySelector('.short-field-dialog')).toBe(form)
      expect(request.mock.calls.filter(([input]) => /^\/api\/(mkdir|rename)/.test(String(input)))).toHaveLength(1)
      resolveWrite(new Response(JSON.stringify({ success: true }), { status: 200 }))
      await settle()
      expect(host.querySelector('.short-field-dialog')).toBeNull()
      expect(document.querySelector('.app-toast.success')?.textContent).toContain(kind === 'folder' ? '文件夹已创建' : '重命名成功')
    } finally {
      resolveWrite(new Response(JSON.stringify({ success: true }), { status: 200 }))
      app.unmount()
      await settle()
    }
  })

  it('shows unknown legacy batch results consistently in the toast and detail dialog', async () => {
    const request = vi.fn((input: RequestInfo | URL) => {
      if (String(input).startsWith('/api/batch/delete')) return Promise.resolve(new Response(JSON.stringify({
        success: 0, failed: 0, results: [
          { path: 'one.txt', status: 409, code: 'operation_result_unknown', message: 'Verify first' },
        ],
      }), { status: 409 }))
      if (input === '/api/me') return Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false })))
      return Promise.resolve(new Response(JSON.stringify({
        storage_id: 'primary', storages: [], current_path: '', can_write: true, next_cursor: null,
        entries: [{ name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false }],
      })))
    })
    vi.stubGlobal('fetch', request)
    const { app, host } = mountView()
    try {
      await settle()
      host.querySelector<HTMLElement>('.file-row')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
      await nextTick()
      const remove = [...host.querySelectorAll<HTMLButtonElement>('.context-menu .menu-item')].find(button => button.textContent?.includes('删除'))!
      remove.click()
      await nextTick()
      const confirm = document.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')
      expect(confirm).toBeDefined()
      confirm!.click()
      await settle()
      await nextTick()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('结果待确认 1 项')
      expect(document.querySelector('.app-toast.success')).toBeNull()
      expect(host.querySelector('.result-modal p')?.textContent).toContain('结果待确认 1 项')
      expect(host.querySelectorAll('.result-row')).toHaveLength(1)
      expect(host.querySelector('.result-row')?.textContent).toContain('one.txt')
      expect(host.querySelector('.result-row')?.textContent).toContain('Verify first (operation_result_unknown)')
      expect(request.mock.calls.filter(([input]) => String(input).startsWith('/api/batch/delete'))).toHaveLength(1)
    } finally {
      app.unmount()
    }
  })

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

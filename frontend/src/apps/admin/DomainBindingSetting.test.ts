import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { DomainBindingView } from '../../shared/api/admin'
import DomainBindingSetting from './DomainBindingSetting.vue'

const empty: DomainBindingView = { binding: null, source: 'none' }
const binding = { public_url: 'https://cloud.example.com' }
const response = (body: unknown) => new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' } })
const settle = async () => { await new Promise(resolve => setTimeout(resolve, 0)); await nextTick() }

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })

function mount(initial = empty) {
  const host = document.createElement('div'); document.body.append(host)
  const app = createApp(DomainBindingSetting, { initial }); app.mount(host)
  return { app, host }
}

describe('DomainBindingSetting', () => {
  it('disables confirmation while reading and aborts only that read when unmounted', async () => {
    const fetch = vi.fn().mockImplementation(() => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount()
    let mounted = true
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
      await nextTick()
      expect([...host.querySelectorAll<HTMLButtonElement>('.modal-actions button')].every(button => button.disabled)).toBe(true)
      expect(host.querySelector<HTMLButtonElement>('.drawer-close')!.disabled).toBe(true)
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      expect(fetch).toHaveBeenCalledTimes(1)
      const signal = fetch.mock.calls[0]![1].signal as AbortSignal
      expect(signal.aborted).toBe(false)
      app.unmount()
      mounted = false
      await settle()
      expect(signal.aborted).toBe(true)
      expect(document.querySelector('.app-toast')).toBeNull()
    } finally { if (mounted) app.unmount() }
  })

  it.each([
    { operation: 'save', outcome: 'success' },
    { operation: 'save', outcome: 'failure' },
    { operation: 'remove', outcome: 'success' },
    { operation: 'remove', outcome: 'failure' },
  ] as const)('does not cancel an accepted $operation or show a late $outcome after unmounting', async ({ operation, outcome }) => {
    let finish!: (value: Response) => void
    const fetch = vi.fn().mockResolvedValueOnce(response({ binding, source: 'settings' })).mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount({ binding, source: 'settings' })
    let mounted = true
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
      await settle()
      const input = host.querySelector<HTMLInputElement>('input[type="url"]')!
      input.value = operation === 'remove' ? '' : 'https://other.example.com'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetch).toHaveBeenCalledTimes(2)
      expect(fetch.mock.calls[1]![1].method).toBe(operation === 'save' ? 'PUT' : 'DELETE')
      expect([...host.querySelectorAll<HTMLButtonElement>('.modal-actions button')].every(button => button.disabled)).toBe(true)
      expect(host.querySelector('.domain-remove')).toBeNull()
      const signal = fetch.mock.calls[1]![1].signal as AbortSignal
      app.unmount()
      mounted = false
      expect(signal.aborted).toBe(false)
      finish(outcome === 'success' ? response(operation === 'save' ? { binding, source: 'settings' } : empty)
        : new Response(JSON.stringify({ error: { message: 'Persistence failed' } }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
      await settle()
      expect(signal.aborted).toBe(false)
      expect(fetch).toHaveBeenCalledTimes(2)
      expect(document.querySelector('.app-toast')).toBeNull()
    } finally { if (mounted) app.unmount() }
  })

  it('keeps a rejected URL draft for explicit retry and displays the confirmed normalized binding', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(response(empty))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Persistence rejected' } }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(response({ binding, source: 'settings' }))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount()
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
      await settle()
      const input = host.querySelector<HTMLInputElement>('input[type="url"]')!
      input.value = '  https://CLOUD.example.com:443/  '
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await settle()
      expect(input.value).toBe('https://CLOUD.example.com:443/')
      expect(fetch).toHaveBeenCalledTimes(2)
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Persistence rejected')
      expect(host.querySelector('.domain-open-link')).toBeNull()
      expect(host.querySelector<HTMLButtonElement>('.modal-actions .btn:not(.secondary)')!.disabled).toBe(false)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await settle()
      expect(fetch).toHaveBeenCalledTimes(3)
      expect(fetch).toHaveBeenLastCalledWith('/api/admin/domain-binding', expect.objectContaining({ method: 'PUT', body: JSON.stringify({ public_url: 'https://CLOUD.example.com:443/' }) }))
      expect(input.value).toBe(binding.public_url)
      expect(host.querySelector('.setting-row')?.textContent).toContain(binding.public_url)
      expect(host.querySelector('.domain-open-link')).toBeNull()
    } finally { app.unmount() }
  })

  it('opens one read even when the edit action is clicked twice before rendering', async () => {
    let finish!: (value: Response) => void
    const fetch = vi.fn().mockImplementation(() => new Promise<Response>(resolve => { finish = resolve }))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount()
    try {
      const edit = host.querySelector<HTMLButtonElement>('.setting-row button')!
      edit.click()
      edit.click()
      expect(fetch).toHaveBeenCalledTimes(1)
      finish(response(empty))
      await settle()
      expect(host.querySelector<HTMLInputElement>('input[type="url"]')?.value).toBe('')
    } finally { app.unmount() }
  })

  it('saves an HTTPS domain immediately with one confirmation', async () => {
    const saved: DomainBindingView = { binding, source: 'settings' }
    const fetch = vi.fn().mockResolvedValueOnce(response(empty)).mockResolvedValueOnce(response(saved))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount()
    expect(fetch).not.toHaveBeenCalled()
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    expect(host.querySelector('.settings-drawer-head')?.textContent).not.toContain('返回')
    expect(host.querySelector('details')).toBeNull()
    expect(host.querySelector('summary')).toBeNull()
    expect(host.textContent).toContain('清空后确认即可解除绑定')
    expect(host.textContent).not.toContain('验证期')
    const inputs = host.querySelectorAll<HTMLInputElement>('.domain-binding-form input')
    expect(inputs).toHaveLength(1)
    inputs[0]!.value = binding.public_url; inputs[0]!.dispatchEvent(new Event('input'))
    await nextTick()
    expect(fetch).toHaveBeenCalledTimes(1)
    host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await settle()
    expect(fetch).toHaveBeenCalledTimes(2)
    expect(fetch).toHaveBeenLastCalledWith('/api/admin/domain-binding', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ public_url: binding.public_url }),
    }))
    expect(document.querySelector('.app-toast.success')?.textContent).toContain('域名绑定已立即生效')
    expect(host.querySelector<HTMLInputElement>('input[type="url"]')?.value).toBe(binding.public_url)
    expect(host.querySelector('.domain-open-link')).toBeNull()
    expect([...host.querySelectorAll('.modal-actions button')].map(button => button.textContent)).toEqual(['取消', '确认'])
    app.unmount()
  })

  it('cancel and close discard drafts without sending a mutation', async () => {
    const saved: DomainBindingView = { binding, source: 'settings' }
    const fetch = vi.fn().mockImplementation(() => Promise.resolve(response(saved)))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount(saved)
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    expect(host.querySelector('details')).toBeNull()
    const input = host.querySelector<HTMLInputElement>('input[type="url"]')!
    input.value = 'https://other.example.com'; input.dispatchEvent(new Event('input')); await nextTick()
    host.querySelector<HTMLButtonElement>('.modal-actions .secondary')!.click(); await nextTick()
    expect(host.querySelector('.settings-drawer')).toBeNull()
    expect(fetch).toHaveBeenCalledTimes(1)
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    expect(host.querySelector<HTMLInputElement>('input[type="url"]')?.value).toBe(binding.public_url)
    host.querySelector<HTMLButtonElement>('.drawer-close')!.click(); await nextTick()
    expect(fetch).toHaveBeenCalledTimes(2)
    expect(fetch.mock.calls.every(call => call[1] === undefined || call[1].method === undefined)).toBe(true)
    app.unmount()
  })

  it('removes a persisted binding by clearing the address and confirming once', async () => {
    const saved: DomainBindingView = { binding, source: 'settings' }
    const fetch = vi.fn().mockResolvedValueOnce(response(saved)).mockResolvedValueOnce(response(empty))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount(saved)
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    expect(host.querySelector('.domain-remove')).toBeNull()
    const input = host.querySelector<HTMLInputElement>('input[type="url"]')!
    expect(input.required).toBe(false)
    input.value = '   '; input.dispatchEvent(new Event('input')); await nextTick()
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(host.querySelector('.setting-row')?.textContent).toContain(binding.public_url)
    host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await settle()
    expect(fetch).toHaveBeenLastCalledWith('/api/admin/domain-binding', expect.objectContaining({ method: 'DELETE' }))
    expect(document.querySelector('.app-toast.success')?.textContent).toContain('已解除域名绑定')
    expect(input.value).toBe('')
    expect(host.querySelector('.setting-row')?.textContent).toContain('未设置')
    expect(host.querySelector('.domain-open-link')).toBeNull()
    app.unmount()
  })

  it('keeps the committed binding and empty draft when removal fails', async () => {
    const saved: DomainBindingView = { binding, source: 'settings' }
    const fetch = vi.fn().mockResolvedValueOnce(response(saved))
      .mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Removal rejected' } }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount(saved)
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
      const input = host.querySelector<HTMLInputElement>('input[type="url"]')!
      input.value = ''; input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await settle()
      expect(fetch).toHaveBeenCalledTimes(2)
      expect(input.value).toBe('')
      expect(host.querySelector('.setting-row')?.textContent).toContain(binding.public_url)
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Removal rejected')
      expect(document.querySelector('.app-toast.success')).toBeNull()
    } finally { app.unmount() }
  })

  it('shows failed persistence or validation without claiming the binding succeeded', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(response(empty)).mockRejectedValueOnce(new Error('配置保存失败'))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount()
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await settle()
    expect(document.querySelector('.app-toast.error[role="alert"]')?.textContent).toContain('配置保存失败')
    expect(host.querySelector('.domain-open-link')).toBeNull()
    app.unmount()
  })
})

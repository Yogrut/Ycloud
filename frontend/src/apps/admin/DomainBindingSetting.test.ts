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
    expect(host.textContent).toContain('不填写则使用 HTTP')
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
    expect(host.querySelector<HTMLAnchorElement>('.domain-open-link')?.href).toBe('https://cloud.example.com/admin/account')
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

  it('requires confirmation before removing a persisted binding', async () => {
    const saved: DomainBindingView = { binding, source: 'settings' }
    const fetch = vi.fn().mockResolvedValueOnce(response(saved)).mockResolvedValueOnce(response(empty))
    vi.stubGlobal('fetch', fetch)
    const { app, host } = mount(saved)
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click(); await settle()
    host.querySelector<HTMLButtonElement>('.domain-remove')!.click(); await nextTick()
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(host.textContent).toContain('恢复 HTTP 访问')
    host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true })); await settle()
    expect(fetch).toHaveBeenLastCalledWith('/api/admin/domain-binding', expect.objectContaining({ method: 'DELETE' }))
    expect(document.querySelector('.app-toast.success')?.textContent).toContain('已解除域名绑定')
    app.unmount()
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

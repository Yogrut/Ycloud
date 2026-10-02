import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import ProtectionView from './ProtectionView.vue'

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

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountProtection(host: HTMLElement, currentInfo = info, onSaved?: (message: string) => void) {
  const app = createApp(ProtectionView, { info: currentInfo, onSaved })
  app.mount(host)
  return app
}

describe('ProtectionView', () => {
  it.each([
    [0, '4', { admin_login_failures: 4 }, 3, 10],
    [1, '15', { admin_login_block_seconds: 900 }, 5, 1440],
    [2, '6', { web_login_failures: 6 }, 3, 20],
    [3, '20', { web_login_block_seconds: 1200 }, 5, 1440],
  ] as const)('binds editor %i to only its own setting and limits', async (index, value, body, min, max) => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountProtection(host)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[index]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="number"]')!
      expect(input.min).toBe(String(min))
      expect(input.max).toBe(String(max))
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
      input.value = value
      input.dispatchEvent(new Event('input'))
      await nextTick()
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual(body)
    } finally { app.unmount() }
  })

  it('preserves rejected edits for manual retry and suppresses simultaneous submissions', async () => {
    let finish!: (response: Response) => void
    const fetchMock = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountProtection(host, info, saved)
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="number"]')!
      input.value = '4'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(saved).not.toHaveBeenCalled()
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
      finish(new Response(JSON.stringify({ error: { code: 'access_denied', message: 'Protection edit denied' } }), { status: 403, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Protection edit denied')
      expect(host.querySelector('.settings-drawer')).not.toBeNull()
      expect(input.value).toBe('4')
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(saved).not.toHaveBeenCalled()
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      for (const call of fetchMock.mock.calls) expect(JSON.parse(call[1].body)).toEqual({ admin_login_failures: 4 })
      expect(saved).toHaveBeenCalledTimes(1)
      expect(host.querySelector('.settings-drawer')).toBeNull()
    } finally { app.unmount() }
  })

  it('discards cancelled changes when reopening and never sends invalid settings', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountProtection(host)
    try {
      const edit = host.querySelector<HTMLButtonElement>('.setting-row button')!
      edit.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="number"]')!
      input.value = '11'
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('3 到 10')
      expect(fetchMock).not.toHaveBeenCalled()
      host.querySelector<HTMLButtonElement>('.modal-actions .secondary')!.click()
      await nextTick()
      edit.click()
      await nextTick()
      expect(host.querySelector<HTMLInputElement>('input[type="number"]')!.value).toBe('3')
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
    } finally { app.unmount() }
  })

  it('can change failure limits without changing existing second-precision durations', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountProtection(host, { ...info, admin_login_block_seconds: 301, web_login_block_seconds: 307 })
    try {
      host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="number"]')!
      input.value = '4'
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/settings', expect.objectContaining({
        body: JSON.stringify({ admin_login_failures: 4 }),
      }))
      expect(host.querySelector('.settings-drawer')).toBeNull()
    } finally { app.unmount() }
  })

  it('owns and saves sign-in failure limits', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountProtection(host)
    await nextTick()

    expect(host.querySelector('#protection-title')?.textContent).toBe('登录保护')
    host.querySelector<HTMLButtonElement>('.setting-row button')!.click()
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="number"]')
    if (!input) throw new Error('failure limit input not found')
    input.value = '4'
    input.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/settings', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({
        admin_login_failures: 4,
      }),
    }))
    app.unmount()
  })
})

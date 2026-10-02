import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import LimitsView from './LimitsView.vue'

// This suite covers the pre-existing transfer controls; TrafficPanel has its own suite.
vi.mock('./TrafficPanel.vue', () => ({ default: { template: '<div />' } }))

const GIB = 1024 ** 3
const info: AdminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [],
  max_upload_bytes: 5 * GIB, max_upload_batch_bytes: 20 * GIB, max_upload_batch_entries: 1000,
  max_archive_bytes: 3 * GIB, max_archive_entries: 1000,
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

function mountLimits(host: HTMLElement, currentInfo = info, onSaved?: (message: string) => void) {
  const app = createApp(LimitsView, { info: currentInfo, onSaved })
  app.mount(host)
  return app
}

describe('LimitsView', () => {
  it('submits only an explicitly edited rate while preserving the other precise rate', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const preciseInfo = { ...info, upload_rate_bytes_per_sec: 1024 ** 2 + 1, download_rate_bytes_per_sec: 2 * 1024 ** 2 + 3 }
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host, preciseInfo)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[4]!.click()
      await nextTick()
      const rate = host.querySelector<HTMLInputElement>('.settings-drawer input')!
      expect(rate.value).toBe('1')
      rate.value = '1.5'
      rate.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ upload_rate_bytes_per_sec: 1.5 * 1024 ** 2 })
      expect([...host.querySelectorAll('.setting-row-value')].map(item => item.textContent)).toContain('1.5 MiB/s')
      expect(host.querySelector('.settings-drawer')).toBeNull()
      expect(preciseInfo.upload_rate_bytes_per_sec).toBe(1024 ** 2 + 1)
    } finally { app.unmount() }
  })

  it('suppresses duplicate saves and keeps rejected edits for an explicit retry', async () => {
    let finish!: (response: Response) => void
    const fetchMock = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host, info, saved)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[7]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.settings-drawer input')!
      input.value = '999'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(saved).not.toHaveBeenCalled()
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
      finish(new Response(JSON.stringify({ error: { code: 'access_denied', message: 'Limit edit denied' } }), { status: 403, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(host.querySelector('.settings-drawer')).not.toBeNull()
      expect(input.value).toBe('999')
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Limit edit denied')
      expect(saved).not.toHaveBeenCalled()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      for (const call of fetchMock.mock.calls) expect(JSON.parse(call[1].body)).toEqual({ max_archive_entries: 999 })
      expect(saved).toHaveBeenCalledTimes(1)
      expect(host.querySelector('.settings-drawer')).toBeNull()
      expect(host.querySelectorAll('.setting-row-value')[7]!.textContent).toBe('999')
    } finally { app.unmount() }
  })

  it('keeps cancelled rate changes local and restores the persisted value when reopened', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host, { ...info, upload_rate_bytes_per_sec: 1024 ** 2 + 1 })
    try {
      const edit = host.querySelectorAll<HTMLButtonElement>('.setting-row button')[4]!
      edit.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.settings-drawer input')!
      input.value = '2'
      input.dispatchEvent(new Event('input'))
      await nextTick()
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(false)
      host.querySelector<HTMLButtonElement>('.modal-actions .secondary')!.click()
      await nextTick()
      edit.click()
      await nextTick()
      expect(host.querySelector<HTMLInputElement>('.settings-drawer input')!.value).toBe('1')
      expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true)
      expect(fetchMock).not.toHaveBeenCalled()
    } finally { app.unmount() }
  })

  it('does not round or resubmit unchanged rates when saving an unrelated entry limit', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const preciseInfo = { ...info, upload_rate_bytes_per_sec: 1024 ** 2 + 1, download_rate_bytes_per_sec: 2 * 1024 ** 2 + 3 }
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host, preciseInfo)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[7]!.click()
      await nextTick()
      const entries = host.querySelector<HTMLInputElement>('.settings-drawer input')!
      entries.value = '999'
      entries.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ max_archive_entries: 999 })
    } finally { app.unmount() }
  })

  it('renders persisted byte limits as readable GiB values', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host)
    await nextTick()

    expect([...host.querySelectorAll('.setting-row-value')].map(el => el.textContent)).toEqual(['统一设置', '5 GiB', '20 GiB', '1000', '0 MiB/s', '0 MiB/s', '3 GiB', '1000'])
    expect(host.querySelector('.setting-row-label')?.textContent).toBe('流量限制')
    expect(host.querySelector('.settings-drawer')).toBeNull()
    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[1]!.click()
    await nextTick()
    expect(host.querySelector<HTMLInputElement>('.settings-drawer input')?.value).toBe('5')
    expect(host.querySelectorAll('.settings-drawer input')).toHaveLength(1)
    app.unmount()
  })

  it('submits exact integer byte and entry limits', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host)
    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[1]!.click()
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('.settings-drawer input')!
    input.value = '6'
    input.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/limits', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({
        max_upload_bytes: 6 * GIB,
      }),
    }))
    expect(host.querySelector('.settings-drawer')).toBeNull()
    app.unmount()
  })

  it('rejects an invalid entry count before contacting the server', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host)
    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[7]!.click()
    await nextTick()
    const entries = host.querySelector<HTMLInputElement>('.settings-drawer input')
    if (!entries) throw new Error('entry limit input missing')
    entries.value = '100001'
    entries.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(document.querySelector('.app-toast.error')?.textContent).toContain('必须在 1 到 100000 之间')
    app.unmount()
  })

  it('preserves exact server bytes for displayed values that were not edited', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const preciseInfo = { ...info, max_upload_bytes: 1024 ** 2, max_archive_bytes: 3 * GIB + 1234 }
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host, preciseInfo)
    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[7]!.click()
    await nextTick()
    const entries = host.querySelector<HTMLInputElement>('.settings-drawer input')
    if (!entries) throw new Error('entry limit input missing')
    entries.value = '999'
    entries.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    const options = fetchMock.mock.calls[0]?.[1] as RequestInit
    expect(JSON.parse(String(options.body))).toEqual({
      max_archive_entries: 999,
    })
    app.unmount()
  })
})

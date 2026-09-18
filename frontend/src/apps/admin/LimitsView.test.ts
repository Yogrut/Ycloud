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
  storage_instances: [{ id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,
  default_storage_id: 'primary',
  local_storage_path: './storage',
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountLimits(host: HTMLElement, currentInfo = info) {
  const app = createApp(LimitsView, { info: currentInfo })
  app.mount(host)
  return app
}

describe('LimitsView', () => {
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
        max_upload_batch_bytes: 20 * GIB,
        max_upload_batch_entries: 1000,
        max_archive_bytes: 3 * GIB,
        max_archive_entries: 1000,
        upload_rate_bytes_per_sec: 0,
        download_rate_bytes_per_sec: 0,
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
      max_upload_bytes: preciseInfo.max_upload_bytes,
      max_upload_batch_bytes: preciseInfo.max_upload_batch_bytes,
      max_upload_batch_entries: preciseInfo.max_upload_batch_entries,
      max_archive_bytes: preciseInfo.max_archive_bytes,
      max_archive_entries: 999,
      upload_rate_bytes_per_sec: 0,
      download_rate_bytes_per_sec: 0,
    })
    app.unmount()
  })
})

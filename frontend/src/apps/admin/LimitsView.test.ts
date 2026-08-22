import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import LimitsView from './LimitsView.vue'

const GIB = 1024 ** 3
const info: AdminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [],
  max_upload_bytes: 5 * GIB, max_archive_bytes: 3 * GIB, max_archive_entries: 1000,
  upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: 0,
  admin_login_failures: 3, web_login_failures: 5,
  admin_login_block_seconds: 3600, web_login_block_seconds: 3600,
  security_log_retention_days: 7, security_log_max_entries: 5000,
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

    const inputs = host.querySelectorAll<HTMLInputElement>('input')
    expect(inputs[0]?.value).toBe('5')
    expect(inputs[1]?.value).toBe('0')
    expect(inputs[2]?.value).toBe('0')
    expect(inputs[3]?.value).toBe('3')
    expect(inputs[4]?.value).toBe('1000')
    expect(host.textContent).toContain('磁盘始终保留安全余量')
    expect(host.querySelector('.limits-grid')).not.toBeNull()
    expect(host.querySelectorAll('.limits-grid > .compact-field')).toHaveLength(5)
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
    const inputs = host.querySelectorAll<HTMLInputElement>('input')
    if (!inputs[0] || !inputs[3] || !inputs[4]) throw new Error('limit input missing')
    inputs[0].value = '6'
    inputs[0].dispatchEvent(new Event('input'))
    inputs[3].value = '2.5'
    inputs[3].dispatchEvent(new Event('input'))
    inputs[4].value = '750'
    inputs[4].dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/limits', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({
        max_upload_bytes: 6 * GIB,
        max_archive_bytes: 2.5 * GIB,
        max_archive_entries: 750,
        upload_rate_bytes_per_sec: 0,
        download_rate_bytes_per_sec: 0,
      }),
    }))
    expect((host.querySelector('button[type="submit"]') as HTMLButtonElement).disabled).toBe(true)
    app.unmount()
  })

  it('rejects an invalid entry count before contacting the server', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountLimits(host)
    const entries = host.querySelectorAll<HTMLInputElement>('input')[4]
    if (!entries) throw new Error('entry limit input missing')
    entries.value = '5001'
    entries.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.textContent).toContain('必须在 1 到 5000 之间')
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
    const entries = host.querySelectorAll<HTMLInputElement>('input')[4]
    if (!entries) throw new Error('entry limit input missing')
    entries.value = '999'
    entries.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    const options = fetchMock.mock.calls[0]?.[1] as RequestInit
    expect(JSON.parse(String(options.body))).toEqual({
      max_upload_bytes: preciseInfo.max_upload_bytes,
      max_archive_bytes: preciseInfo.max_archive_bytes,
      max_archive_entries: 999,
      upload_rate_bytes_per_sec: 0,
      download_rate_bytes_per_sec: 0,
    })
    app.unmount()
  })
})

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
  storage_instances: [{ id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,
  default_storage_id: 'primary',
  local_storage_path: './storage',
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('ProtectionView', () => {
  it('owns and saves sign-in failure limits', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(ProtectionView, { info })
    app.mount(host)
    await nextTick()

    expect(host.querySelector('#protection-title')?.textContent).toBe('登录保护')
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
        web_login_failures: 5,
        admin_login_block_seconds: 3600,
        web_login_block_seconds: 3600,
      }),
    }))
    app.unmount()
  })
})

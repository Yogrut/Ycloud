import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import UsersView from './UsersView.vue'

const info: AdminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [],
  max_upload_bytes: 1024, max_archive_bytes: 1024, max_archive_entries: 100,
  upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: 0,
  admin_login_failures: 3, web_login_failures: 5,
  admin_login_block_seconds: 3600, web_login_block_seconds: 3600,
  security_log_retention_days: 7, security_log_max_entries: 5000,
  storage_instances: [
    { id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 },
    { id: 'archive', name: '归档盘', is_default: false, ready: true, backend: { type: 'local', mount_id: 'archive', path: '/mnt/archive', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 },
  ],
  pending_storage_instance: null, default_storage_id: 'primary', local_storage_path: './storage',
  user_accounts: [{
    id: 'reader', username: 'reader', enabled: true,
    permissions: [{ storage_id: 'primary', browse: true, download: true, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false }],
  }],
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountUsers(host: HTMLElement) {
  const app = createApp(UsersView, { info })
  app.mount(host)
  return app
}

describe('UsersView', () => {
  it('shows each storage as one fully expanded permission row with localized names', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountUsers(host)
    ;(host.querySelector('.admin-pane-head .btn') as HTMLButtonElement).click()
    await nextTick()

    const grants = host.querySelectorAll('.storage-grant-row')
    expect(grants).toHaveLength(2)
    expect(grants[0]?.children).toHaveLength(9)
    expect(grants[0]?.textContent).toContain('本地存储')
    expect(grants[0]?.textContent).not.toContain('Local storage')
    expect(grants[0]?.textContent).toContain('允许访问')
    expect(grants[1]?.textContent).toContain('允许访问')
    expect(grants[0]?.querySelectorAll('.storage-grant-option')).toHaveLength(7)
    expect(host.querySelector('.permission-table')).toBeNull()
    expect(host.querySelector('details')).toBeNull()
    app.unmount()
  })

  it('edits storage permissions without reading or resubmitting the existing password', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(info.user_accounts?.[0]), {
      status: 200, headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountUsers(host)
    await nextTick()

    expect(host.textContent).toContain('账号只能由管理员创建、授权和修改密码')
    ;(host.querySelector('.user-row .btn') as HTMLButtonElement).click()
    await nextTick()
    const password = host.querySelector<HTMLInputElement>('input[type="password"]')
    expect(password?.value).toBe('')
    ;(host.querySelector('.user-editor') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    const request = fetchMock.mock.calls[0]?.[1] as RequestInit
    const body = JSON.parse(String(request.body)) as Record<string, unknown>
    expect(body.password).toBeUndefined()
    expect(body.permissions).toEqual(info.user_accounts?.[0]?.permissions)
    app.unmount()
  })
})

import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import UsersView from './UsersView.vue'

vi.mock('../../shared/api/admin', async (importOriginal) => {
  const original = await importOriginal<typeof import('../../shared/api/admin')>()
  return { ...original, getTraffic: vi.fn().mockResolvedValue({
    settings: { users: { reader: { enabled: true, upload: 1024, download: 2048 } } },
    users: { reader: { upload: 100, download: 200 } },
  }) }
})

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
  it('keeps switches as drafts and clears action permissions when storage access is turned off', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('{}', {headers:{'Content-Type':'application/json'}}))
    vi.stubGlobal('fetch',fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountUsers(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('.user-traffic-quota')?.textContent).toBe('↓2K | ↑1K')
    expect(host.querySelector('.user-traffic-quota')?.classList.contains('status-pill')).toBe(true)
    host.querySelector<HTMLButtonElement>('[aria-label="编辑"]')!.click(); await nextTick()
    host.querySelector<HTMLButtonElement>('.storage-grant-heading')!.click(); await nextTick()
    const access = host.querySelector<HTMLInputElement>('.storage-access-option input[role="switch"]')!
    expect(access.checked).toBe(true)
    access.click(); await nextTick()
    const permissions = host.querySelectorAll<HTMLInputElement>('.storage-grant-details .storage-grant-option input')
    expect(permissions).toHaveLength(7)
    permissions.forEach(input => { expect(input.checked).toBe(false); expect(input.disabled).toBe(true) })
    access.click(); await nextTick()
    permissions.forEach(input => { expect(input.checked).toBe(false); expect(input.disabled).toBe(false) })
    permissions[1]!.click(); await nextTick()
    host.querySelector<HTMLInputElement>('[role="switch"][aria-label="启用账号"]')!.click(); await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
    host.querySelector('form')!.dispatchEvent(new Event('submit',{cancelable:true}))
    await new Promise(resolve => setTimeout(resolve,0))
    const body = JSON.parse(fetchMock.mock.calls[0]![1].body)
    expect(body.enabled).toBe(false)
    expect(body.traffic).toEqual({enabled:true,upload:1024,download:2048})
    expect(body.permissions[0]).toMatchObject({browse:true,upload:true,download:false})
    app.unmount()
  })
  it('sets an independent traffic allowance while creating a user', async () => {
    const created = { id: 'new-user', username: 'new-user', enabled: true, permissions: [] }
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(created), {headers:{'Content-Type':'application/json'}}))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountUsers(host)
    host.querySelector<HTMLButtonElement>('.admin-pane-head .btn')!.click(); await nextTick()
    const fields = host.querySelectorAll<HTMLInputElement>('.user-basic-grid input')
    fields[0]!.value = 'new-user'; fields[0]!.dispatchEvent(new Event('input'))
    fields[1]!.value = 'abcdefghijkl'; fields[1]!.dispatchEvent(new Event('input'))
    host.querySelector<HTMLInputElement>('.traffic-quota input[role="switch"]')!.click(); await nextTick()
    const limits = host.querySelectorAll<HTMLInputElement>('.traffic-quota input[type="number"]')
    limits[0]!.value = '1'; limits[0]!.dispatchEvent(new Event('input')); await nextTick()
    limits[1]!.value = '2'; limits[1]!.dispatchEvent(new Event('input')); await nextTick()
    host.querySelector('form')!.dispatchEvent(new Event('submit',{cancelable:true}))
    await new Promise(resolve => setTimeout(resolve,0))
    const body = JSON.parse(fetchMock.mock.calls[0]![1].body)
    expect(body.traffic).toEqual({enabled:true,upload:1024 ** 3,download:2 * 1024 ** 3})
    app.unmount()
  })
  it('uses text actions and an in-page confirmation; cancel never deletes', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountUsers(host)
    const trigger = host.querySelector<HTMLButtonElement>('.user-row button[aria-label="删除"]')!
    expect(trigger.textContent).toBe('删除')
    expect(host.querySelector('[aria-label="编辑"]')?.textContent).toBe('编辑')
    expect(host.querySelector('.row-actions svg')).toBeNull()
    trigger.focus(); trigger.click(); await nextTick(); await nextTick()
    expect(host.querySelector('.confirmation-target')?.textContent).toContain('reader')
    expect(host.textContent).toContain('存储文件不会被删除')
    expect(document.activeElement).toBe(host.querySelector('.confirmation-actions .secondary'))
    host.querySelector<HTMLButtonElement>('.confirmation-actions .secondary')!.click(); await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.querySelector('[role="dialog"]')).toBeNull()
    expect(document.activeElement).toBe(trigger)
    app.unmount()
  })

  it('shows delete failures above the dialog and prevents duplicate requests while busy', async () => {
    let finish!: (value: Response) => void
    const fetchMock = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountUsers(host)
    host.querySelector<HTMLButtonElement>('[aria-label="删除"]')!.click(); await nextTick()
    const confirm = host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!
    confirm.click(); confirm.click(); await nextTick()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(confirm.disabled).toBe(true)
    host.querySelector('.confirmation-dialog')!.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))
    await nextTick()
    expect(host.querySelector('.confirmation-dialog')).not.toBeNull()
    finish(new Response(JSON.stringify({error: {message: '删除失败测试'}}), {status:500, headers:{'Content-Type':'application/json'}}))
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(document.querySelector('.app-toast.error[role="alert"]')?.textContent).toContain('删除失败测试')
    confirm.click(); await new Promise(resolve => setTimeout(resolve, 0))
    expect(fetchMock).toHaveBeenLastCalledWith('/api/admin/users/reader', expect.objectContaining({method:'DELETE'}))
    expect(host.querySelector('.confirmation-dialog')).toBeNull()
    app.unmount()
  })
  it('starts collapsed and expands only one storage at a time', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountUsers(host)
    ;(host.querySelector('.admin-pane-head .btn') as HTMLButtonElement).click()
    await nextTick()

    const grants = host.querySelectorAll('.storage-grant-row')
    expect(grants).toHaveLength(2)
    expect(host.querySelectorAll('.storage-grant-details')).toHaveLength(0)
    expect(grants[0]?.textContent).toContain('本地存储')
    expect(grants[0]?.textContent).not.toContain('Local storage')
    expect(grants[0]?.textContent).toContain('未授权')
    grants[0]?.querySelector<HTMLButtonElement>('.storage-grant-heading')!.click(); await nextTick()
    expect(grants[0]?.querySelectorAll('.storage-grant-option')).toHaveLength(7)
    grants[1]?.querySelector<HTMLButtonElement>('.storage-grant-heading')!.click(); await nextTick()
    expect(grants[0]?.querySelector('.storage-grant-details')).toBeNull()
    expect(grants[1]?.querySelectorAll('.storage-grant-option')).toHaveLength(7)
    expect(host.querySelectorAll('.storage-grant-details')).toHaveLength(1)
    expect([...host.querySelectorAll('.modal-actions button')].map(item => item.textContent?.trim())).toEqual(['取消','确认'])
    app.unmount()
  })

  it('searches by name and path, preserving changes and the original storage index', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('{}',{headers:{'Content-Type':'application/json'}}))
    vi.stubGlobal('fetch',fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const app = mountUsers(host)
    host.querySelector<HTMLButtonElement>('[aria-label="编辑"]')!.click(); await nextTick()
    const search = host.querySelector<HTMLInputElement>('.storage-grant-search')!
    search.value='/mnt/archive'; search.dispatchEvent(new Event('input')); await nextTick()
    expect(host.querySelectorAll('.storage-grant-row')).toHaveLength(1)
    expect(host.querySelector('.storage-grant-heading')?.textContent).toContain('归档盘')
    host.querySelector<HTMLButtonElement>('.storage-grant-heading')!.click(); await nextTick()
    host.querySelector<HTMLInputElement>('.storage-access-option input')!.click(); await nextTick()
    host.querySelector<HTMLInputElement>('.storage-grant-option input')!.click(); await nextTick()
    search.value='本地'; search.dispatchEvent(new Event('input')); await nextTick()
    expect(host.querySelector('.storage-grant-heading')?.textContent).toContain('本地存储')
    search.value='missing'; search.dispatchEvent(new Event('input')); await nextTick()
    expect(host.textContent).toContain('没有匹配的存储')
    search.value=''; search.dispatchEvent(new Event('input')); await nextTick()
    expect(host.querySelector<HTMLInputElement>('.storage-grant-details .storage-grant-option input')?.checked).toBe(true)
    expect(fetchMock).not.toHaveBeenCalled()
    host.querySelector('form')!.dispatchEvent(new Event('submit',{cancelable:true}))
    await new Promise(resolve=>setTimeout(resolve,0))
    const body=JSON.parse(fetchMock.mock.calls[0]![1].body)
    expect(body.permissions).toHaveLength(2)
    expect(body.permissions[0]).toEqual(info.user_accounts![0]!.permissions[0])
    expect(body.permissions[1]).toMatchObject({storage_id:'archive',browse:true,download:true})
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

    expect(host.textContent).toContain('用户只能由管理员创建、授权和修改密码')
    ;(host.querySelector('.user-row button[aria-label="编辑"]') as HTMLButtonElement).click()
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

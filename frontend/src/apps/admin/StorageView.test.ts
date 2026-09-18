import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { LocalMountView, StorageInstanceView } from '../../shared/api/admin'
import StorageView from './StorageView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

const localInstance: StorageInstanceView = {
  id: 'primary',
  name: '本地存储',
  is_default: false,
  enabled: true,
  allow_guest_access: true,
  status: 'enabled',
  ready: true,
  backend: { type: 'local', mount_id: 'primary', path: './storage', capacity_limit_bytes: null },
  usage_bytes: 0,
  reserved_bytes: 0,
}

const localMounts: LocalMountView[] = [
  { mount_id: 'primary', name: '主数据盘', path: './storage', storage_id: 'primary', ready: true, total_bytes: 40 * (1024 ** 3), available_bytes: 30 * (1024 ** 3) },
  { mount_id: 'archive', name: '归档盘', path: '/mnt/archive', storage_id: null, ready: true, total_bytes: 100 * (1024 ** 3), available_bytes: 90 * (1024 ** 3) },
]

function mountStorage(host: HTMLElement, pendingInstance: StorageInstanceView | null = null, instances: StorageInstanceView[] = [localInstance]) {
  const changed = vi.fn()
  const app = createApp(StorageView, {
    instances,
    pendingInstance,
    localMounts,
    onChanged: changed,
  })
  app.mount(host)
  return { app, changed }
}

function response(body: unknown = { success: true }, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

function button(host: HTMLElement, label: string): HTMLButtonElement | undefined {
  return [...host.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent?.trim().includes(label))
}

function setInput(input: HTMLInputElement | null, value: string): void {
  if (!input) throw new Error(`input missing for ${value}`)
  input.value = value
  input.dispatchEvent(new Event('input'))
}

function setLabeledInput(host: HTMLElement, label: string, value: string): void {
  const field = [...host.querySelectorAll<HTMLLabelElement>('.storage-form > label')]
    .find(item => item.textContent?.trim().startsWith(label))
    ?.querySelector('input')
  setInput(field ?? null, value)
}

async function chooseSelect(host: HTMLElement, selector: string, label: string): Promise<void> {
  host.querySelector<HTMLButtonElement>(`${selector} .app-select-trigger`)?.click()
  await nextTick()
  const option = [...host.querySelectorAll<HTMLButtonElement>(`${selector} .app-select-option`)]
    .find(item => item.textContent?.includes(label))
  if (!option) throw new Error(`select option missing for ${label}`)
  option.click()
  await nextTick()
}

describe('StorageView', () => {
  it('requires guest access before downloads and clears downloads when access is disabled', async () => {
    const host = document.createElement('div'); document.body.append(host)
    const { app } = mountStorage(host)
    button(host, '新建存储')!.click(); await nextTick()
    const access = host.querySelector<HTMLInputElement>('input[role="switch"][aria-label="允许访客访问"]')!
    const download = host.querySelector<HTMLInputElement>('input[role="switch"][aria-label="允许访客下载"]')!
    expect(download.disabled).toBe(true)
    expect(download.checked).toBe(false)
    access.click(); await nextTick()
    expect(download.disabled).toBe(false)
    expect(download.checked).toBe(false)
    download.click(); await nextTick()
    expect(download.checked).toBe(true)
    access.click(); await nextTick()
    expect(download.disabled).toBe(true)
    expect(download.checked).toBe(false)
    access.click(); await nextTick()
    expect(download.checked).toBe(false)
    app.unmount()
  })
  it('requires in-page confirmation before deleting storage configuration', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, {status:204}))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const { app, changed } = mountStorage(host)
    host.querySelector<HTMLButtonElement>('[aria-label="删除存储"]')!.click(); await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.querySelector('.confirmation-target')?.textContent).toContain('本地存储')
    expect(host.querySelector('.confirmation-detail')?.textContent).toContain('文件不会被删除')
    host.querySelector<HTMLButtonElement>('.confirmation-actions .secondary')!.click(); await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
    host.querySelector<HTMLButtonElement>('[aria-label="删除存储"]')!.click(); await nextTick()
    host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/primary', expect.objectContaining({method:'DELETE'}))
    expect(changed).toHaveBeenCalledWith(expect.stringContaining('文件未被删除'))
    expect(host.querySelector('.confirmation-dialog')).toBeNull()
    app.unmount()
  })
  it('keeps setup collapsed until New storage is selected', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    await nextTick()

    expect(host.querySelectorAll('.storage-source-row')).toHaveLength(1)
    expect(host.textContent).toContain('启用')
    expect(host.textContent).toContain('访客可访问')
    expect(host.querySelectorAll('.storage-provider')).toHaveLength(0)
    expect(button(host, '新建存储')?.querySelector('svg')).toBeNull()
    expect(host.querySelector('button[aria-label="编辑存储"]')?.textContent).toBe('编辑')
    expect(host.querySelector('button[aria-label="删除存储"]')?.textContent).toBe('删除')
    expect(host.querySelector('.record-text-btn svg')).toBeNull()
    expect(host.querySelector('button[aria-label="设为默认存储"] svg')).not.toBeNull()

    button(host, '新建存储')?.click()
    await nextTick()
    expect(host.querySelectorAll('.storage-provider')).toHaveLength(5)
    expect([...host.querySelectorAll('.modal-actions button')].map(item => item.textContent?.trim())).toEqual(['测试连接', '取消', '确认'])
    expect(host.textContent).toContain('MinIO / RustFS')
    expect(host.textContent).toContain('S3 通用协议')
    app.unmount()
  })

  it('distinguishes capacity reconciliation in progress from waiting for reconciliation', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host, null, [
        { ...localInstance, capacity_accurate: false, capacity_reconciling: false },
        { ...localInstance, id: 'checking', name: '核对盘', capacity_accurate: false, capacity_reconciling: true },
      ])
    await nextTick()
    expect(host.textContent).toContain('待核对')
    expect(host.textContent).toContain('核对中')
    app.unmount()
  })

  it('shows recorded cleanup debt without claiming an incomplete inventory is exact', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host, null, [
      { ...localInstance, cleanup_pending_bytes: 4096, cleanup_debt_complete: true },
      { ...localInstance, id: 'partial-debt', name: '待核对盘', cleanup_pending_bytes: 2048, cleanup_debt_complete: false },
    ])
    await nextTick()
    expect(host.textContent).toContain('待回收（上限）4.00 KiB')
    expect(host.textContent).toContain('回收积压仍在核对（已登记上限 2.00 KiB）')
    app.unmount()
  })

  it('shows bounded upload and copy staging cleanup backlogs', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host, null, [{
      ...localInstance,
      staging_cleanup_pending_uploads: 1,
      staging_cleanup_pending_copies: 2,
      staging_cleanup_failed_attempts: 3,
    }])
    await nextTick()
    expect(host.textContent).toContain('待清理暂存：上传 1，复制 2，累计失败重试 3 次')
    app.unmount()
  })

  it('shows read-only S3 orphan inventory without claiming it was cleaned', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host, null, [{
      ...localInstance,
      id: 's3-storage',
      name: '对象存储',
      backend: {
        type: 's3',
        provider: 's3_compatible',
        endpoint: 'https://s3.example.com',
        bucket: 'bucket',
        region: 'us-east-1',
        prefix: 'tenant/',
        addressing_style: 'path',
        has_access_key_id: true,
        has_secret_access_key: true,
        capacity_limit_bytes: null,
      },
      s3_orphan_uploads: 2,
      s3_orphan_backups: 1,
    }])
    await nextTick()
    expect(host.textContent).toContain('待处理对象存储暂存：上传 2，备份 1')
    expect(host.textContent).not.toContain('已清理')
    app.unmount()
  })

  it('shows bounded runtime S3 recovery state and its latest safe failure', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host, null, [{
      ...localInstance,
      id: 's3-recovery',
      name: '恢复中的对象存储',
      s3_recovery_pending_records: 2,
      s3_recovery_oldest_pending_seconds: 125,
      s3_recovery_consecutive_failures: 3,
      s3_recovery_last_failure: '对象存储删除失败',
      s3_recovery_running: false,
    }])
    await nextTick()
    expect(host.textContent).toContain('对象存储待恢复：2 条责任记录')
    expect(host.textContent).toContain('最早已等待 2 分钟')
    expect(host.textContent).toContain('连续失败 3 次')
    expect(host.textContent).toContain('对象存储删除失败')
    app.unmount()
  })

  it('uses virtual-hosted addressing for official cloud profiles', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    button(host, '新建存储')?.click()
    await nextTick()
    host.querySelectorAll<HTMLButtonElement>('.storage-provider')[1]?.click()
    await nextTick()

    const addressing = [...host.querySelectorAll<HTMLLabelElement>('.storage-form > label')]
      .find(item => item.textContent?.includes('寻址方式'))?.querySelector<HTMLButtonElement>('.app-select-trigger')
    expect(addressing?.textContent).toContain('Virtual Hosted')
    expect(addressing?.disabled).toBe(true)
    app.unmount()
  })

  it('tests a declared local mount before adding it', async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response()))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    button(host, '新建存储')?.click()
    await nextTick()

    button(host, '测试连接')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local/test', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ path: '/mnt/archive' }),
    }))
    expect(document.querySelector('[role="status"].app-toast.success')?.textContent).toBe('测试成功：存储连接与读写能力验证通过')
    expect(host.querySelector('.admin-form-error')).toBeNull()
    expect(changed).not.toHaveBeenCalled()
    host.querySelectorAll<HTMLButtonElement>('.storage-provider')[1]?.click()
    await nextTick()
    expect(document.querySelector('.app-toast.success')).toBeNull()
    app.unmount()
  })

  it('shows connection errors as alerts and replaces success when a retry fails', async () => {
    const message = 'S3 Endpoint 必须是长度不超过 2048 字符的完整地址'
    const fetchMock = vi.fn().mockResolvedValue(response({ message }, 400))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div'); document.body.append(host)
    const { app, changed } = mountStorage(host)
    button(host, '新建存储')?.click(); await nextTick()
    host.querySelectorAll<HTMLButtonElement>('.storage-provider')[4]?.click(); await nextTick()
    button(host, '测试连接')?.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(document.querySelector('[role="alert"].app-toast.error')?.textContent).toBe(`测试失败：${message}`)
    expect(host.querySelector('.storage-form [role="alert"]')).toBeNull()
    expect(host.querySelector('.storage-form [role="status"]')).toBeNull()
    expect(document.querySelector('.app-toast.success')).toBeNull()
    expect(changed).not.toHaveBeenCalled()

    fetchMock.mockResolvedValue(response({ success: true }))
    button(host, '测试连接')?.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('.admin-form-error')).toBeNull()
    expect(document.querySelector('.app-toast.success')).not.toBeNull()

    fetchMock.mockResolvedValue(response({ success: false }))
    button(host, '测试连接')?.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(document.querySelector('[role="alert"].app-toast.error')?.textContent).toBe('测试失败：存储连接测试失败')
    expect(document.querySelector('.app-toast.success')).toBeNull()
    app.unmount()
  })

  it('sets a ready storage as the default', async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response()))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)

    host.querySelector<HTMLButtonElement>('button[aria-label="设为默认存储"]')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/primary/default', expect.objectContaining({ method: 'PUT' }))
    expect(changed).toHaveBeenCalledWith('默认存储已更新')
    app.unmount()
  })

  it('reconfigures the complete local storage settings', async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response()))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    host.querySelector<HTMLButtonElement>('button[aria-label="编辑存储"]')?.click()
    await nextTick()
    setLabeledInput(host, '存储名称', '主资料盘')
    await chooseSelect(host, '.local-storage-path', '/mnt/archive')
    setInput(host.querySelector('.local-capacity-input'), '800')
    button(host, '确认')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({ storage_id: 'primary', name: '主资料盘', path: '/mnt/archive', capacity_limit_bytes: 800 * (1024 ** 3), enabled: true, allow_guest_access: true, allow_guest_download: true }),
    }))
    expect(changed).toHaveBeenCalledWith('存储设置已保存')
    app.unmount()
  })

  it('adds a local storage from an administrator-entered path without importing files', async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response({ storage_id: 'local-archive' }, 201)))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    button(host, '新建存储')?.click()
    await nextTick()
    setInput(host.querySelector('.local-storage-name'), '冷数据归档')
    expect(host.querySelector('.local-storage-path .app-select-value')?.textContent).toContain('/mnt/archive')
    setInput(host.querySelector('.local-capacity-input'), '80')
    button(host, '确认')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ path: '/mnt/archive', name: '冷数据归档', capacity_limit_bytes: 80 * (1024 ** 3), enabled: true, allow_guest_access: false, allow_guest_download: false }),
    }))
    expect(changed).toHaveBeenCalledWith(expect.stringContaining('文件未被移动或删除'))
    app.unmount()
  })

  it('verifies, saves, and activates S3 from one editor action', async () => {
    const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(response()))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    button(host, '新建存储')?.click()
    await nextTick()
    host.querySelectorAll<HTMLButtonElement>('.storage-provider')[3]?.click()
    await nextTick()
    setInput(host.querySelector('.local-storage-name'), '家庭 RustFS')
    setLabeledInput(host, 'Endpoint', 'https://rustfs.internal.example')
    setLabeledInput(host, 'Bucket', 'ycloud')
    setLabeledInput(host, 'Region', 'us-east-1')
    setLabeledInput(host, 'Prefix', 'files/')
    setLabeledInput(host, 'Access Key ID', 'access-id')
    setLabeledInput(host, 'Secret Access Key', 'secret-value')
    button(host, '确认')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    const pendingCall = fetchMock.mock.calls.find(call => call[0] === '/api/admin/storage/pending')
    expect(JSON.parse(pendingCall?.[1]?.body as string)).toMatchObject({
      name: '家庭 RustFS', provider: 'minio', endpoint: 'https://rustfs.internal.example', bucket: 'ycloud',
      enabled: true, allow_guest_access: false, access_key_id: 'access-id', secret_access_key: 'secret-value',
    })
    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/activate', expect.objectContaining({ method: 'POST' }))
    expect(changed).toHaveBeenCalledWith(expect.stringContaining('存储源已添加'))
    app.unmount()
  })
})

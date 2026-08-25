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
  is_default: true,
  ready: true,
  backend: { type: 'local', path: './storage', capacity_limit_bytes: null },
  usage_bytes: 0,
  reserved_bytes: 0,
}

const localMounts: LocalMountView[] = [
  { mount_id: 'primary', name: '主数据盘', path: './storage', storage_id: 'primary', ready: true, total_bytes: 40 * (1024 ** 3), available_bytes: 30 * (1024 ** 3) },
  { mount_id: 'archive', name: '归档盘', path: '/mnt/archive', storage_id: null, ready: true, total_bytes: 100 * (1024 ** 3), available_bytes: 90 * (1024 ** 3) },
]

function mountStorage(host: HTMLElement, pendingInstance: StorageInstanceView | null = null) {
  const changed = vi.fn()
  const app = createApp(StorageView, {
    instances: [localInstance],
    pendingInstance,
    defaultStorageId: 'primary',
    localMounts,
    onChanged: changed,
  })
  app.mount(host)
  return { app, changed }
}

describe('StorageView', () => {
  it('offers local storage and four constrained S3 profiles', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    await nextTick()

    expect(host.querySelectorAll('.storage-provider')).toHaveLength(5)
    expect(host.textContent).toContain('本地存储')
    expect(host.textContent).toContain('阿里云 OSS')
    expect(host.textContent).toContain('腾讯云 COS')
    expect(host.textContent).toContain('MinIO / RustFS')
    expect(host.textContent).toContain('S3 通用协议')
    expect(host.querySelectorAll('.local-mount-card')).toHaveLength(2)
    expect(host.textContent).toContain('已声明 2 个挂载地址')
    expect(host.querySelector<HTMLInputElement>('.local-path-input')?.value).toBe('./storage')
    expect(host.textContent).toContain('LOCAL_STORAGE_MOUNTS')
    app.unmount()
  })

  it('forces virtual-hosted addressing for official cloud profiles', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    const providerButtons = host.querySelectorAll<HTMLButtonElement>('.storage-provider')
    providerButtons[1]?.click()
    await nextTick()

    const addressing = host.querySelector<HTMLSelectElement>('select')
    expect(addressing?.value).toBe('virtual_hosted')
    expect(addressing?.disabled).toBe(true)
    app.unmount()
  })

  it('stages a local logical capacity without changing the storage path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    await nextTick()

    const path = host.querySelector<HTMLInputElement>('.local-path-input')
    const capacity = host.querySelector<HTMLInputElement>('.local-capacity-input')
    if (!capacity) throw new Error('local capacity input missing')
    capacity.value = '800'
    capacity.dispatchEvent(new Event('input'))
    await nextTick()
    host.querySelector<HTMLButtonElement>('.local-save-button')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(path?.value).toBe('./storage')
    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({ storage_id: 'primary', capacity_limit_bytes: 800 * (1024 ** 3) }),
    }))
    expect(changed).toHaveBeenCalledWith('本地存储容量设置已更新')
    app.unmount()
  })

  it('adds a declared unused mount as a separate local storage source', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ storage_id: 'local-archive' }), {
      status: 201,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    await nextTick()

    host.querySelector<HTMLButtonElement>('[data-mount-id="archive"]')?.click()
    await nextTick()
    const name = host.querySelector<HTMLInputElement>('.local-storage-name')
    const capacity = host.querySelector<HTMLInputElement>('.local-capacity-input')
    if (!name || !capacity) throw new Error('local storage fields missing')
    name.value = '冷数据归档'
    name.dispatchEvent(new Event('input'))
    capacity.value = '80'
    capacity.dispatchEvent(new Event('input'))
    host.querySelector<HTMLButtonElement>('.local-add-button')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ mount_id: 'archive', name: '冷数据归档', capacity_limit_bytes: 80 * (1024 ** 3) }),
    }))
    expect(changed).toHaveBeenCalledWith('本地存储源已添加；现有文件和默认存储均未改变')
    app.unmount()
  })

  it('fully verifies and saves a MinIO or RustFS endpoint as pending', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host)
    host.querySelectorAll<HTMLButtonElement>('.storage-provider')[3]?.click()
    await nextTick()
    const inputs = host.querySelectorAll<HTMLInputElement>('.storage-form input')
    if (!inputs[0] || !inputs[1] || !inputs[2] || !inputs[3] || !inputs[4] || !inputs[5] || !inputs[6] || !inputs[7]) {
      throw new Error('storage setup input missing')
    }
    const values = ['家庭 RustFS', '0', 'https://rustfs.internal.example', 'ycloud', 'us-east-1', 'files/', 'access-id', 'secret-value']
    inputs.forEach((input, index) => {
      input.value = values[index] ?? input.value
      input.dispatchEvent(new Event('input'))
    })
    ;(host.querySelector('.storage-form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/pending', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({
        name: '家庭 RustFS',
        provider: 'minio',
        endpoint: 'https://rustfs.internal.example',
        bucket: 'ycloud',
        region: 'us-east-1',
        prefix: 'files/',
        addressing_style: 'path',
        access_key_id: 'access-id',
        secret_access_key: 'secret-value',
        capacity_limit_bytes: null,
      }),
    }))
    expect(inputs[7].value).toBe('')
    expect(changed).toHaveBeenCalledWith('S3 完整能力验证通过，已保存为待添加存储')
    app.unmount()
  })

  it('requires explicit confirmation before activating a pending backend', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountStorage(host, {
      id: 'rustfs',
      name: '家庭 RustFS',
      is_default: false,
      ready: true,
      backend: {
        type: 's3',
        provider: 'minio',
        endpoint: 'https://rustfs.internal.example',
        bucket: 'ycloud',
        region: 'us-east-1',
        prefix: 'files/',
        addressing_style: 'path',
        has_access_key_id: true,
        has_secret_access_key: true,
        capacity_limit_bytes: null,
      },
      usage_bytes: 0,
      reserved_bytes: 0,
    })
    await nextTick()

    expect(host.textContent).toContain('待添加：家庭 RustFS')
    expect(host.textContent).not.toContain('pending-secret')
    const activate = Array.from(host.querySelectorAll<HTMLButtonElement>('.storage-pending-actions button'))
      .find(button => button.textContent?.trim() === '添加存储')
    activate?.click()
    await nextTick()
    expect(host.querySelector('[role="dialog"]')).not.toBeNull()
    expect(fetchMock).not.toHaveBeenCalled()

    const confirm = Array.from(host.querySelectorAll<HTMLButtonElement>('.modal-actions button'))
      .find(button => button.textContent?.trim() === '确认添加')
    confirm?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/activate', expect.objectContaining({
      method: 'POST',
      credentials: 'same-origin',
    }))
    expect(changed).toHaveBeenCalledWith('新存储已添加；默认存储和现有文件均未改变')
    app.unmount()
  })
})

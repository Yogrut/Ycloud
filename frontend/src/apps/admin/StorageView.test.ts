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

function mountStorage(host: HTMLElement, pendingInstance: StorageInstanceView | null = null) {
  const changed = vi.fn()
  const app = createApp(StorageView, {
    instances: [localInstance],
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
    expect(host.querySelector('button[aria-label="编辑存储"] svg')).not.toBeNull()

    button(host, '新建存储')?.click()
    await nextTick()
    expect(host.querySelectorAll('.storage-provider')).toHaveLength(5)
    expect(host.textContent).toContain('MinIO / RustFS')
    expect(host.textContent).toContain('S3 通用协议')
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
    expect(changed).toHaveBeenCalledWith('存储连接与读写能力验证通过')
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
    button(host, '保存')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({ storage_id: 'primary', name: '主资料盘', path: '/mnt/archive', capacity_limit_bytes: 800 * (1024 ** 3), enabled: true, allow_guest_access: true }),
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
    button(host, '保存')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/local', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ path: '/mnt/archive', name: '冷数据归档', capacity_limit_bytes: 80 * (1024 ** 3), enabled: true, allow_guest_access: false }),
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
    button(host, '保存')?.click()
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

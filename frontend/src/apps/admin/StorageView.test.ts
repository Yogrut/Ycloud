import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import StorageView from './StorageView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountStorage(host: HTMLElement) {
  const tested = vi.fn()
  const app = createApp(StorageView, {
    backend: { type: 'local' },
    onTested: tested,
  })
  app.mount(host)
  return { app, tested }
}

describe('StorageView', () => {
  it('offers four constrained S3 profiles and keeps long path fields full width', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    await nextTick()

    expect(host.querySelectorAll('.storage-provider')).toHaveLength(4)
    expect(host.textContent).toContain('阿里云 OSS')
    expect(host.textContent).toContain('腾讯云 COS')
    expect(host.textContent).toContain('MinIO / RustFS')
    expect(host.textContent).toContain('S3 通用协议')
    expect(host.querySelectorAll('.storage-form > .storage-wide')).toHaveLength(4)
    expect(host.textContent).toContain('测试不会保存密钥或切换当前存储')
    app.unmount()
  })

  it('forces virtual-hosted addressing for official cloud profiles', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountStorage(host)
    const providerButtons = host.querySelectorAll<HTMLButtonElement>('.storage-provider')
    providerButtons[0]?.click()
    await nextTick()

    const addressing = host.querySelector<HTMLSelectElement>('select')
    expect(addressing?.value).toBe('virtual_hosted')
    expect(addressing?.disabled).toBe(true)
    app.unmount()
  })

  it('tests a MinIO or RustFS endpoint without persisting or activating it', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, tested } = mountStorage(host)
    const inputs = host.querySelectorAll<HTMLInputElement>('.storage-form input')
    if (!inputs[0] || !inputs[1] || !inputs[2] || !inputs[3] || !inputs[4] || !inputs[5]) {
      throw new Error('storage setup input missing')
    }
    const values = ['https://rustfs.internal.example', 'ycloud', 'us-east-1', 'files/', 'access-id', 'secret-value']
    inputs.forEach((input, index) => {
      input.value = values[index] ?? ''
      input.dispatchEvent(new Event('input'))
    })
    ;(host.querySelector('.storage-form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/test', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({
        provider: 'minio',
        endpoint: 'https://rustfs.internal.example',
        bucket: 'ycloud',
        region: 'us-east-1',
        prefix: 'files/',
        addressing_style: 'path',
        access_key_id: 'access-id',
        secret_access_key: 'secret-value',
      }),
    }))
    expect(inputs[5].value).toBe('')
    expect(tested).toHaveBeenCalledWith('S3 连接与最小列举权限验证通过')
    app.unmount()
  })
})

import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { StorageInstanceView, WebDavMountView } from '../../shared/api/admin'
import WebDavView from './WebDavView.vue'

const mountView: WebDavMountView = {
  id: 'share-1',
  storage_id: 'primary',
  name: 'media',
  path: 'files',
  username: 'dav-user',
  webdav_enabled: true,
  has_password: true,
  readonly: false,
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountWebDav(host: HTMLElement, mounts: WebDavMountView[] = []) {
  const storages: StorageInstanceView[] = [{
    id: 'primary', name: '本地存储', is_default: true, ready: true,
    backend: { type: 'local', path: './storage', capacity_limit_bytes: null },
    usage_bytes: 0, reserved_bytes: 0,
  }]
  const changed = vi.fn()
  const app = createApp(WebDavView, { mounts, storages, defaultStorageId: 'primary', onChanged: changed })
  app.mount(host)
  return { app, changed }
}

function respondJson(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } })
}

describe('WebDavView', () => {
  it('renders connection, storage and permission state without a password value', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountWebDav(host, [mountView])
    await nextTick()

    expect(host.textContent).toContain('/dav/media')
    expect(host.textContent).toContain('存储路径 /files')
    expect(host.textContent).toContain('密码已设置')
    expect(host.textContent).not.toContain('secure-dav-password')
    app.unmount()
  })

  it('creates an enabled mount with a normalized storage path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson(mountView))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountWebDav(host)
    ;(host.querySelector('.webdav-head .btn') as HTMLButtonElement).click()
    await nextTick()
    const inputs = host.querySelectorAll<HTMLInputElement>('.webdav-form-grid input')
    if (!inputs[0] || !inputs[1] || !inputs[2] || !inputs[3]) throw new Error('WebDAV inputs missing')
    inputs[0].value = 'media'
    inputs[0].dispatchEvent(new Event('input'))
    inputs[1].value = '\\files//'
    inputs[1].dispatchEvent(new Event('input'))
    inputs[2].value = 'dav-user'
    inputs[2].dispatchEvent(new Event('input'))
    inputs[3].value = '安全挂载密码12345678'
    inputs[3].dispatchEvent(new Event('input'))
    ;(host.querySelector('.webdav-modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/shares', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({
        storage_id: 'primary', name: 'media', path: 'files', username: 'dav-user', password: '安全挂载密码12345678',
        webdav_enabled: true, readonly: false,
      }),
    }))
    expect(changed).toHaveBeenCalledWith('WebDAV 挂载“media”已创建')
    app.unmount()
  })

  it('shows long storage and connection paths in full-width fields', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountWebDav(host)
    ;(host.querySelector('.webdav-head .btn') as HTMLButtonElement).click()
    await nextTick()

    const fullFields = host.querySelectorAll('.webdav-form-grid .full-field')
    const connection = host.querySelector<HTMLInputElement>('.connection-path-field input')
    expect(fullFields).toHaveLength(3)
    expect(connection?.readOnly).toBe(true)
    expect(connection?.value).toBe('/dav/挂载名称')
    app.unmount()
  })

  it('updates only an explicitly changed field and preserves the password mask', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({ ...mountView, readonly: true }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountWebDav(host, [mountView])
    ;(host.querySelector('.webdav-actions .btn') as HTMLButtonElement).click()
    await nextTick()
    const options = host.querySelectorAll<HTMLInputElement>('.webdav-options input')
    if (!options[1]) throw new Error('readonly input missing')
    options[1].checked = true
    options[1].dispatchEvent(new Event('change'))
    ;(host.querySelector('.webdav-modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/shares/share-1', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ readonly: true }),
    }))
    app.unmount()
  })

  it('requires independent credentials when WebDAV is enabled', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountWebDav(host)
    ;(host.querySelector('.webdav-head .btn') as HTMLButtonElement).click()
    await nextTick()
    const nameInput = host.querySelector<HTMLInputElement>('.webdav-form-grid input')
    if (!nameInput) throw new Error('name input missing')
    nameInput.value = 'open'
    nameInput.dispatchEvent(new Event('input'))
    ;(host.querySelector('.webdav-modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.textContent).toContain('启用 WebDAV 必须设置用户名和密码')
    app.unmount()
  })

  it('allows clearing a stored password only while disabling the mount', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({
      ...mountView, webdav_enabled: false, has_password: false,
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountWebDav(host, [mountView])
    ;(host.querySelector('.webdav-actions .btn') as HTMLButtonElement).click()
    await nextTick()
    const fields = host.querySelectorAll<HTMLInputElement>('.webdav-form-grid input')
    const enabled = host.querySelector<HTMLInputElement>('.webdav-options input')
    if (!fields[3] || !enabled) throw new Error('password or enabled input missing')
    fields[3].value = ''
    fields[3].dispatchEvent(new Event('input'))
    enabled.checked = false
    enabled.dispatchEvent(new Event('change'))
    ;(host.querySelector('.webdav-modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/shares/share-1', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ password: '', webdav_enabled: false }),
    }))
    app.unmount()
  })

  it('deletes only the mount configuration after confirmation', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountWebDav(host, [mountView])
    ;(host.querySelector('.webdav-actions .danger-outline') as HTMLButtonElement).click()
    await nextTick()
    ;(host.querySelector('.modal .btn.danger') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/shares/share-1', expect.objectContaining({ method: 'DELETE' }))
    expect(changed).toHaveBeenCalledWith('WebDAV 挂载“media”已删除')
    expect(host.querySelector('.modal')).toBeNull()
    app.unmount()
  })
})

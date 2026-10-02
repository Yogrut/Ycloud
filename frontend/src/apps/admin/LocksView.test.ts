import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { FolderLockView, StorageInstanceView } from '../../shared/api/admin'
import LocksView from './LocksView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountLocks(host: HTMLElement, locks: FolderLockView[] = []) {
  const storages: StorageInstanceView[] = [{
    id: 'primary', name: '本地存储', enabled: true, ready: true,
    backend: { type: 'local', path: './storage', capacity_limit_bytes: null },
    usage_bytes: 0, reserved_bytes: 0,
  }]
  const changed = vi.fn()
  const app = createApp(LocksView, { locks, storages, onChanged: changed })
  app.mount(host)
  return { app, changed }
}

function respondJson(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } })
}

describe('LocksView', () => {
  it('shares button and request differences without trimming filename whitespace or submitting the mask', async () => {
    const original = { id: 'lock-1', storage_id: 'primary', path: 'files', revision: 'edit-revision' }
    const fetchMock = vi.fn().mockResolvedValue(respondJson(original))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host, [original])
    try {
      host.querySelector<HTMLButtonElement>('[aria-label="编辑文件夹锁"]')!.click()
      await nextTick()
      const path = host.querySelector<HTMLInputElement>('.modal input')!
      const confirm = host.querySelector<HTMLButtonElement>('.modal-actions button[type="submit"]')!
      const form = host.querySelector('form')!
      path.value = '\\files//'
      path.dispatchEvent(new Event('input'))
      await nextTick()
      expect(confirm.disabled).toBe(true)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      expect(fetchMock).not.toHaveBeenCalled()
      path.value = '/files/ child /'
      path.dispatchEvent(new Event('input'))
      await nextTick()
      expect(confirm.disabled).toBe(false)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ expected_revision: 'edit-revision', path: 'files/ child ' })
      expect(changed).toHaveBeenCalledWith('文件夹锁 /files/ child  已更新')
      expect(original.path).toBe('files')
    } finally { app.unmount() }
  })

  it('rejects clearing a lock password and counts replacement Unicode characters', async () => {
    const original = { id: 'lock-1', storage_id: 'primary', path: 'files', revision: 'edit-revision' }
    const fetchMock = vi.fn().mockResolvedValue(respondJson(original))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountLocks(host, [original])
    try {
      host.querySelector<HTMLButtonElement>('[aria-label="编辑文件夹锁"]')!.click()
      await nextTick()
      const password = host.querySelector<HTMLInputElement>('input[type="password"]')!
      const form = host.querySelector('form')!
      password.value = ''
      password.dispatchEvent(new Event('input'))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).not.toHaveBeenCalled()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('文件夹锁密码至少需要 8 位')
      password.value = '🔒'.repeat(7)
      password.dispatchEvent(new Event('input'))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).not.toHaveBeenCalled()
      password.value = '🔒'.repeat(8)
      password.dispatchEvent(new Event('input'))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ expected_revision: 'edit-revision', password: '🔒'.repeat(8) })
    } finally { app.unmount() }
  })

  it('suppresses duplicate updates and keeps failed edits open for explicit retry', async () => {
    let finish!: (response: Response) => void
    const original = { id: 'lock-1', storage_id: 'primary', path: 'files', revision: 'edit-revision' }
    const fetchMock = vi.fn().mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
      .mockResolvedValueOnce(respondJson(original))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host, [original])
    try {
      host.querySelector<HTMLButtonElement>('[aria-label="编辑文件夹锁"]')!.click()
      await nextTick()
      const path = host.querySelector<HTMLInputElement>('.modal input')!
      path.value = '/renamed/'
      path.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(changed).not.toHaveBeenCalled()
      expect(host.querySelector<HTMLButtonElement>('.modal-actions button[type="submit"]')!.disabled).toBe(true)
      finish(new Response(JSON.stringify({ error: { code: 'access_denied', message: 'Lock edit denied' } }), { status: 403, headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Lock edit denied')
      expect(host.querySelector('form')).not.toBeNull()
      expect(path.value).toBe('/renamed/')
      expect(changed).not.toHaveBeenCalled()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      for (const call of fetchMock.mock.calls) expect(JSON.parse(call[1].body)).toEqual({ expected_revision: 'edit-revision', path: 'renamed' })
      expect(changed).toHaveBeenCalledTimes(1)
      expect(host.querySelector('form')).toBeNull()
    } finally { app.unmount() }
  })

  it('creates a lock with a normalized path and character-counted password', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({ id: 'lock-1', path: 'test/child' }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host)
    ;(host.querySelector('.locks-head .btn') as HTMLButtonElement).click()
    await nextTick()
    expect(host.querySelector('.compact-editor-modal')).not.toBeNull()
    const inputs = host.querySelectorAll<HTMLInputElement>('.modal input')
    if (!inputs[0] || !inputs[1]) throw new Error('lock inputs missing')
    inputs[0].value = '\\test//child/'
    inputs[0].dispatchEvent(new Event('input'))
    inputs[1].value = '安全密码123456'
    inputs[1].dispatchEvent(new Event('input'))
    ;(host.querySelector('.modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/locks', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ storage_id: 'primary', path: 'test/child', password: '安全密码123456' }),
    }))
    expect(changed).toHaveBeenCalledWith('文件夹锁 /test/child 已创建')
    app.unmount()
  })

  it('edits the path without sending the password mask', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({ id: 'lock-1', path: 'renamed' }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountLocks(host, [{ id: 'lock-1', storage_id: 'primary', path: 'test' }])
    expect(host.querySelector('button[aria-label="编辑文件夹锁"]')?.textContent).toBe('编辑')
    expect(host.querySelector('button[aria-label="删除文件夹锁"]')?.textContent).toBe('删除')
    expect(host.querySelector('.record-text-btn svg')).toBeNull()
    ;(host.querySelector('button[aria-label="编辑文件夹锁"]') as HTMLButtonElement).click()
    await nextTick()
    const pathInput = host.querySelector<HTMLInputElement>('.modal input')
    if (!pathInput) throw new Error('path input missing')
    pathInput.value = '/renamed/'
    pathInput.dispatchEvent(new Event('input'))
    ;(host.querySelector('.modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/locks/lock-1', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ path: 'renamed' }),
    }))
    app.unmount()
  })

  it('rejects the storage root and a short password before contacting the server', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountLocks(host)
    ;(host.querySelector('.locks-head .btn') as HTMLButtonElement).click()
    await nextTick()
    const inputs = host.querySelectorAll<HTMLInputElement>('.modal input')
    if (!inputs[0] || !inputs[1]) throw new Error('lock inputs missing')
    inputs[0].value = '/'
    inputs[0].dispatchEvent(new Event('input'))
    inputs[1].value = '1234567'
    inputs[1].dispatchEvent(new Event('input'))
    ;(host.querySelector('.modal') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(document.querySelector('.app-toast.error')?.textContent).toContain('不能给存储根目录加锁')
    app.unmount()
  })

  it('deletes a lock after confirmation without treating 204 as an error', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host, [{ id: 'lock-1', storage_id: 'primary', path: 'test' }])
    ;(host.querySelector('button[aria-label="删除文件夹锁"]') as HTMLButtonElement).click()
    await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.querySelector('.confirmation-target')?.textContent).toContain('/test')
    ;(host.querySelector('.confirmation-actions .btn:not(.secondary)') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/locks/lock-1', expect.objectContaining({ method: 'DELETE' }))
    expect(changed).toHaveBeenCalledWith('文件夹锁 /test 已删除')
    expect(host.querySelector('.modal')).toBeNull()
    app.unmount()
  })
})

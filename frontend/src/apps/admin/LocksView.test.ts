import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { FolderLockView } from '../../shared/api/admin'
import LocksView from './LocksView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountLocks(host: HTMLElement, locks: FolderLockView[] = []) {
  const changed = vi.fn()
  const app = createApp(LocksView, { locks, onChanged: changed })
  app.mount(host)
  return { app, changed }
}

function respondJson(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } })
}

describe('LocksView', () => {
  it('creates a lock with a normalized path and character-counted password', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({ id: 'lock-1', path: 'test/child' }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host)
    ;(host.querySelector('.locks-head .btn') as HTMLButtonElement).click()
    await nextTick()
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
      body: JSON.stringify({ path: 'test/child', password: '安全密码123456' }),
    }))
    expect(changed).toHaveBeenCalledWith('文件夹锁 /test/child 已创建')
    app.unmount()
  })

  it('edits the path without sending the password mask', async () => {
    const fetchMock = vi.fn().mockResolvedValue(respondJson({ id: 'lock-1', path: 'renamed' }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app } = mountLocks(host, [{ id: 'lock-1', path: 'test' }])
    ;(host.querySelector('.lock-actions .btn') as HTMLButtonElement).click()
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
    expect(host.textContent).toContain('不能给存储根目录加锁')
    app.unmount()
  })

  it('deletes a lock after confirmation without treating 204 as an error', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const { app, changed } = mountLocks(host, [{ id: 'lock-1', path: 'test' }])
    ;(host.querySelector('.lock-actions .danger-outline') as HTMLButtonElement).click()
    await nextTick()
    ;(host.querySelector('.modal .btn.danger') as HTMLButtonElement).click()
    await new Promise(resolve => window.setTimeout(resolve, 0))

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/locks/lock-1', expect.objectContaining({ method: 'DELETE' }))
    expect(changed).toHaveBeenCalledWith('文件夹锁 /test 已删除')
    expect(host.querySelector('.modal')).toBeNull()
    app.unmount()
  })
})

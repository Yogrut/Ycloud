import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import AccountView from './AccountView.vue'

const info: AdminInfo = {
  username: 'ycloud-admin',
  has_global_web_password: true,
  shares: [],
  folder_locks: [],
  login_security: [],
  max_upload_bytes: 1024,
  max_archive_bytes: 1024,
  max_archive_entries: 100,
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountAccount(host: HTMLElement) {
  const app = createApp(AccountView, { info })
  app.mount(host)
  return app
}

describe('AccountView', () => {
  it('does not submit masked passwords when only the username changes', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)

    const username = host.querySelector('input[autocomplete="username"]') as HTMLInputElement
    username.value = 'renamed-admin'
    username.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    const options = fetchMock.mock.calls[0]?.[1] as RequestInit
    expect(JSON.parse(String(options.body))).toEqual({ username: 'renamed-admin' })
    app.unmount()
  })

  it('requires confirmation before removing an existing web password', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)

    const passwords = host.querySelectorAll<HTMLInputElement>('input[type="password"]')
    const webPassword = passwords[1]
    if (!webPassword) throw new Error('web password input not found')
    webPassword.value = ''
    webPassword.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.textContent).toContain('确认移除首页密码')
    app.unmount()
  })
})

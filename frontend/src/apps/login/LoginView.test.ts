import { createApp, nextTick, type App } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { enterGate, getIdentity } from '../../shared/api/auth'
import LoginView from './LoginView.vue'

vi.mock('../../shared/api/auth', () => ({ enterGate: vi.fn(), getIdentity: vi.fn() }))
const apps: App[] = []

function mountLogin() {
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(LoginView)
  apps.push(app)
  app.mount(host)
  return host
}

beforeEach(() => {
  vi.mocked(enterGate).mockResolvedValue(undefined)
  vi.mocked(getIdentity).mockResolvedValue({ logged_in: false, web_password_required: true })
})

afterEach(() => {
  apps.splice(0).forEach(app => app.unmount())
  vi.restoreAllMocks()
  vi.resetAllMocks()
  document.body.replaceChildren()
})

describe('optional login preferences', () => {
  it('detects an existing session even when preference reads and removal fail', async () => {
    vi.spyOn(sessionStorage, 'getItem').mockImplementation(() => { throw new DOMException('Blocked', 'SecurityError') })
    vi.spyOn(sessionStorage, 'removeItem').mockImplementation(() => { throw new DOMException('Blocked', 'SecurityError') })
    vi.mocked(getIdentity).mockResolvedValue({ logged_in: true })
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => {})
    mountLogin()
    await nextTick()
    expect(getIdentity).toHaveBeenCalledOnce()
    expect(replace).toHaveBeenCalledExactlyOnceWith('/browse')
  })

  it('does not report successful authentication as failed when preference removal fails', async () => {
    vi.spyOn(sessionStorage, 'getItem').mockReturnValue('1')
    vi.spyOn(sessionStorage, 'removeItem').mockImplementation(() => { throw new DOMException('Blocked', 'SecurityError') })
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => {})
    const host = mountLogin()
    const input = host.querySelector<HTMLInputElement>('input')!
    input.value = 'valid password'
    input.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))
    await nextTick()
    expect(enterGate).toHaveBeenCalledExactlyOnceWith('valid password')
    expect(replace).toHaveBeenCalledExactlyOnceWith('/browse')
    expect(host.textContent).not.toContain('Blocked')
  })
})

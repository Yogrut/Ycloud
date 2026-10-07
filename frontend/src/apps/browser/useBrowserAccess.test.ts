import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { logout, type BrowserCapabilities } from '../../shared/api/browser'
import { useBrowserAccess } from './useBrowserAccess'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  logout: vi.fn(),
}))
const logoutMock = vi.mocked(logout)
const scopes: EffectScope[] = []

function setup() {
  const scope = effectScope()
  scopes.push(scope)
  const context = {
    storageId: ref('primary'),
    capabilities: ref<BrowserCapabilities>({
      download: true, upload: true, create_directory: true, rename: true,
      move_items: true, copy: true, delete: true,
    }),
    isAdministrator: ref(false),
    navigate: vi.fn().mockResolvedValue(undefined),
    resetAfterSignIn: vi.fn().mockResolvedValue(undefined),
    openPreviewImage: vi.fn().mockReturnValue(false),
    openPreviewFile: vi.fn(), openAccountMenu: vi.fn(),
    disposeListing: vi.fn(), announce: vi.fn(),
  }
  const access = scope.run(() => useBrowserAccess(context))!
  return { context, access, scope }
}

beforeEach(() => {
  logoutMock.mockReset()
  logoutMock.mockResolvedValue({ success: true })
  sessionStorage.removeItem('ycloud-stay-signed-out')
})
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.restoreAllMocks()
  sessionStorage.removeItem('ycloud-stay-signed-out')
})

describe('browser sign out', () => {
  it('waits for confirmation and ignores duplicate clicks before disposing or redirecting', async () => {
    let resolve!: (value: { success: boolean }) => void
    logoutMock.mockImplementationOnce(() => new Promise(done => { resolve = done }))
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => undefined)
    const { access, context } = setup()
    const first = access.signOut()
    await access.signOut()
    expect(access.signingOut.value).toBe(true)
    expect(logoutMock).toHaveBeenCalledTimes(1)
    expect(context.disposeListing).not.toHaveBeenCalled()
    expect(replace).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ycloud-stay-signed-out')).toBeNull()
    resolve({ success: true })
    await first
    expect(access.signingOut.value).toBe(false)
    expect(context.disposeListing).toHaveBeenCalledTimes(1)
    expect(replace).toHaveBeenCalledWith('/')
    expect(sessionStorage.getItem('ycloud-stay-signed-out')).toBe('1')
  })

  it('keeps the current page usable on failure and permits a later explicit sign-out', async () => {
    logoutMock.mockRejectedValueOnce(new Error('Service unavailable'))
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => undefined)
    const { access, context } = setup()
    await access.signOut()
    expect(context.announce).toHaveBeenCalledWith('Service unavailable')
    expect(context.disposeListing).not.toHaveBeenCalled()
    expect(replace).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ycloud-stay-signed-out')).toBeNull()
    expect(access.signingOut.value).toBe(false)
    await access.signOut()
    expect(logoutMock).toHaveBeenCalledTimes(2)
    expect(context.disposeListing).toHaveBeenCalledTimes(1)
    expect(replace).toHaveBeenCalledTimes(1)
  })

  it('does not redirect or dispose a new page after the original scope is gone', async () => {
    let resolve!: (value: { success: boolean }) => void
    logoutMock.mockImplementationOnce(() => new Promise(done => { resolve = done }))
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => undefined)
    const { access, context, scope } = setup()
    const pending = access.signOut()
    scope.stop()
    resolve({ success: true })
    await pending
    await access.signOut()
    expect(logoutMock).toHaveBeenCalledTimes(1)
    expect(context.disposeListing).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
    expect(replace).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ycloud-stay-signed-out')).toBeNull()
  })

  it('still leaves after confirmed logout when browser preference storage is unavailable', async () => {
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => undefined)
    const { access, context } = setup()
    vi.spyOn(sessionStorage, 'setItem').mockImplementation(() => { throw new Error('Storage blocked') })
    await access.signOut()
    expect(context.disposeListing).toHaveBeenCalledTimes(1)
    expect(replace).toHaveBeenCalledWith('/')
    expect(context.announce).not.toHaveBeenCalled()
  })
})

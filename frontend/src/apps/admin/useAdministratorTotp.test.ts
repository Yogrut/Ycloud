import { effectScope, nextTick, ref, type EffectScope } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { disableAdministratorTotp, enableAdministratorTotp, setupAdministratorTotp } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { useAdministratorTotp } from './useAdministratorTotp'

vi.mock('../../shared/api/admin', () => ({
  disableAdministratorTotp: vi.fn(), enableAdministratorTotp: vi.fn(), setupAdministratorTotp: vi.fn(),
}))
const prepare = vi.mocked(setupAdministratorTotp)
const enable = vi.mocked(enableAdministratorTotp)
const disable = vi.mocked(disableAdministratorTotp)
const setupValue = { secret: 'TESTSECRET234567', provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' }
const scopes: EffectScope[] = []

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup(initial?: boolean) {
  const current = ref(initial)
  const onDisabled = vi.fn()
  const scope = effectScope()
  scopes.push(scope)
  const totp = scope.run(() => useAdministratorTotp(() => current.value, onDisabled))!
  return { totp, current, onDisabled, scope }
}

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop()
  prepare.mockReset()
  enable.mockReset()
  disable.mockReset()
  vi.unstubAllGlobals()
  useLocale().set('zh-CN')
})

describe('administrator two-step verification', () => {
  it('isolates drafts and follows the current server flag before recovery codes are shown', async () => {
    const { totp, current } = setup()
    const other = setup(true).totp
    expect(totp.enabled.value).toBe(false)
    expect(other.enabled.value).toBe(true)
    totp.password.value = 'password'
    expect(other.password.value).toBe('')
    current.value = true
    await nextTick()
    expect(totp.enabled.value).toBe(true)
    current.value = false
    await nextTick()
    expect(totp.enabled.value).toBe(false)
  })

  it('prepares a copied setup with the exact untrimmed administrator password', async () => {
    const response = { ...setupValue }
    prepare.mockResolvedValue(response)
    const { totp } = setup()
    totp.password.value = '  current password  '
    await totp.beginSetup()
    expect(prepare).toHaveBeenCalledWith('  current password  ')
    expect(totp.setup.value).toEqual(setupValue)
    response.secret = 'different'
    expect(totp.setup.value?.secret).toBe(setupValue.secret)
    expect(totp.busy.value).toBe(false)
    expect(enable).not.toHaveBeenCalled()
  })

  it('waits for explicit close after enable and keeps recovery codes isolated from the response', async () => {
    const response = { success: true, recovery_codes: ['first-code', 'second-code'] }
    enable.mockResolvedValue(response)
    const { totp, current, onDisabled } = setup()
    totp.setup.value = { ...setupValue }
    totp.password.value = '  current password  '
    totp.code.value = ' 123456 '
    await totp.enable()
    expect(enable).toHaveBeenCalledWith('  current password  ', setupValue.secret, '123456')
    expect(totp.enabled.value).toBe(true)
    expect(totp.recoveryCodes.value).toEqual(['first-code', 'second-code'])
    response.recovery_codes[0] = 'changed'
    expect(totp.recoveryCodes.value[0]).toBe('first-code')
    current.value = true
    await nextTick()
    current.value = false
    await nextTick()
    expect(totp.enabled.value).toBe(true)
    expect(totp.password.value).toBe('')
    expect(totp.code.value).toBe('')
    expect(totp.setup.value).toBeUndefined()
    expect(onDisabled).not.toHaveBeenCalled()
    totp.reset()
    expect(totp.recoveryCodes.value).toEqual([])
  })

  it('clears proofs after confirmed disable and notifies the page only once', async () => {
    disable.mockResolvedValue({ success: true })
    const { totp, onDisabled } = setup(true)
    totp.password.value = '  current password  '
    totp.code.value = ' recovery-code '
    await totp.disable()
    expect(disable).toHaveBeenCalledWith('  current password  ', 'recovery-code')
    expect(totp.enabled.value).toBe(false)
    expect(totp.password.value).toBe('')
    expect(totp.code.value).toBe('')
    expect(onDisabled).toHaveBeenCalledTimes(1)
    await totp.disable()
    expect(disable).toHaveBeenCalledTimes(1)
  })

  it('sends no requests without the required setup and proofs', async () => {
    const { totp } = setup()
    await totp.beginSetup()
    await totp.enable()
    await totp.disable()
    totp.password.value = 'password'
    await totp.enable()
    await totp.disable()
    totp.setup.value = { ...setupValue }
    await totp.enable()
    expect(prepare).not.toHaveBeenCalled()
    expect(enable).not.toHaveBeenCalled()
    expect(disable).not.toHaveBeenCalled()
  })

  it('shares one busy gate across setup, enable and disable and refuses reset while submitting', async () => {
    const pending = deferred<typeof setupValue>()
    prepare.mockReturnValueOnce(pending.promise)
    const { totp } = setup()
    totp.password.value = 'password'
    const first = totp.beginSetup()
    totp.setup.value = { ...setupValue }
    totp.code.value = '123456'
    await totp.beginSetup()
    await totp.enable()
    await totp.disable()
    totp.reset()
    expect(totp.password.value).toBe('password')
    expect(prepare).toHaveBeenCalledTimes(1)
    expect(enable).not.toHaveBeenCalled()
    expect(disable).not.toHaveBeenCalled()
    expect(totp.busy.value).toBe(true)
    pending.resolve(setupValue)
    await first
    expect(totp.busy.value).toBe(false)
  })

  it('does not repeat an enable request while it is pending', async () => {
    const pending = deferred<{ success: boolean; recovery_codes: string[] }>()
    enable.mockReturnValueOnce(pending.promise)
    const { totp } = setup()
    totp.password.value = 'password'
    totp.code.value = '123456'
    totp.setup.value = { ...setupValue }
    const first = totp.enable()
    await totp.enable()
    expect(enable).toHaveBeenCalledTimes(1)
    pending.resolve({ success: true, recovery_codes: ['recovery-code'] })
    await first
    expect(totp.recoveryCodes.value).toEqual(['recovery-code'])
  })

  it('does not repeat a disable request or callback while it is pending', async () => {
    const pending = deferred<{ success: boolean }>()
    disable.mockReturnValueOnce(pending.promise)
    const { totp, onDisabled } = setup(true)
    totp.password.value = 'password'
    totp.code.value = '123456'
    const first = totp.disable()
    await totp.disable()
    expect(disable).toHaveBeenCalledTimes(1)
    expect(onDisabled).not.toHaveBeenCalled()
    pending.resolve({ success: true })
    await first
    expect(onDisabled).toHaveBeenCalledTimes(1)
  })

  it.each(['beginSetup', 'enable', 'disable'] as const)('preserves input and releases busy state after rejected %s without retrying', async operation => {
    const api = { beginSetup: prepare, enable, disable }[operation]
    api.mockRejectedValueOnce(new Error('Proof denied'))
    const { totp, onDisabled } = setup(operation === 'disable')
    totp.password.value = 'password'
    totp.code.value = '123456'
    if (operation === 'enable') totp.setup.value = { ...setupValue }
    await totp[operation]()
    expect(totp.error.value).toBe('Proof denied')
    expect(totp.busy.value).toBe(false)
    expect(totp.password.value).toBe('password')
    expect(totp.code.value).toBe('123456')
    expect(totp.enabled.value).toBe(operation === 'disable')
    expect(totp.recoveryCodes.value).toEqual([])
    expect(api).toHaveBeenCalledTimes(1)
    expect(onDisabled).not.toHaveBeenCalled()
  })

  it.each([
    ['beginSetup', 'Unable to prepare two-step verification'],
    ['enable', 'Unable to enable two-step verification'],
    ['disable', 'Unable to disable two-step verification'],
  ] as const)('uses the existing localized fallback for non-Error %s failures', async (operation, message) => {
    useLocale().set('en')
    const api = { beginSetup: prepare, enable, disable }[operation]
    api.mockRejectedValueOnce('denied')
    const { totp } = setup()
    totp.password.value = 'password'
    totp.code.value = '123456'
    totp.setup.value = { ...setupValue }
    await totp[operation]()
    expect(totp.error.value).toBe(message)
  })

  it('clears a failed attempt when manually retried successfully', async () => {
    prepare.mockRejectedValueOnce(new Error('Denied')).mockResolvedValueOnce(setupValue)
    const { totp } = setup()
    totp.password.value = 'password'
    await totp.beginSetup()
    expect(totp.error.value).toBe('Denied')
    await totp.beginSetup()
    expect(totp.error.value).toBe('')
    expect(totp.setup.value).toEqual(setupValue)
    expect(prepare).toHaveBeenCalledTimes(2)
  })
})

describe('local secrets and copy feedback', () => {
  it('resets local proofs, setup and notices without making a network request', () => {
    const { totp } = setup()
    totp.password.value = 'password'
    totp.code.value = '123456'
    totp.setup.value = { ...setupValue }
    totp.copyNotice.value = { message: 'Copied', kind: 'success' }
    totp.error.value = 'Old error'
    totp.reset()
    expect(totp.password.value).toBe('')
    expect(totp.code.value).toBe('')
    expect(totp.setup.value).toBeUndefined()
    expect(totp.copyNotice.value).toBeUndefined()
    expect(totp.error.value).toBe('')
    expect(prepare).not.toHaveBeenCalled()
  })

  it('copies only the visible secret and displays explicit success and failure', async () => {
    const writeText = vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('Unavailable'))
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    const { totp } = setup()
    await totp.copySecret()
    expect(writeText).not.toHaveBeenCalled()
    totp.setup.value = { ...setupValue }
    await totp.copySecret()
    expect(writeText).toHaveBeenCalledWith(setupValue.secret)
    expect(totp.copyNotice.value).toEqual({ message: '密钥已复制', kind: 'success' })
    await totp.copySecret()
    expect(totp.copyNotice.value?.kind).toBe('error')
  })

  it.each(['success', 'error'] as const)('ignores late clipboard %s after closing the setup', async outcome => {
    const pending = deferred<void>()
    vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn().mockReturnValue(pending.promise) } })
    const { totp } = setup()
    totp.setup.value = { ...setupValue }
    const copy = totp.copySecret()
    totp.reset()
    if (outcome === 'success') pending.resolve()
    else pending.reject(new Error('Unavailable'))
    await copy
    expect(totp.copyNotice.value).toBeUndefined()
  })

  it('discards a previous copy response after replacing the visible secret', async () => {
    const pending = deferred<void>()
    vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn().mockReturnValue(pending.promise) } })
    const { totp } = setup()
    totp.setup.value = { ...setupValue }
    const copy = totp.copySecret()
    totp.setup.value = { ...setupValue, secret: 'OTHERSECRET' }
    pending.resolve()
    await copy
    expect(totp.copyNotice.value).toBeUndefined()
  })

  it('drops pending setup results and blocks new calls after disposal', async () => {
    const pending = deferred<typeof setupValue>()
    prepare.mockReturnValueOnce(pending.promise)
    const { totp, scope } = setup()
    totp.password.value = 'password'
    const request = totp.beginSetup()
    scope.stop()
    expect(totp.password.value).toBe('')
    pending.resolve(setupValue)
    await request
    expect(totp.setup.value).toBeUndefined()
    totp.password.value = 'password'
    await totp.beginSetup()
    await totp.enable()
    await totp.disable()
    await totp.copySecret()
    expect(prepare).toHaveBeenCalledTimes(1)
    expect(enable).not.toHaveBeenCalled()
    expect(disable).not.toHaveBeenCalled()
  })

  it('does not restore recovery codes or secrets from an enable response after disposal', async () => {
    const pending = deferred<{ success: boolean; recovery_codes: string[] }>()
    enable.mockReturnValueOnce(pending.promise)
    const { totp, scope } = setup()
    totp.password.value = 'password'
    totp.code.value = '123456'
    totp.setup.value = { ...setupValue }
    const request = totp.enable()
    scope.stop()
    pending.resolve({ success: true, recovery_codes: ['recovery-code'] })
    await request
    expect(totp.password.value).toBe('')
    expect(totp.code.value).toBe('')
    expect(totp.setup.value).toBeUndefined()
    expect(totp.recoveryCodes.value).toEqual([])
    expect(totp.enabled.value).toBe(false)
  })

  it('does not notify an unmounted page after a submitted disable completes', async () => {
    const pending = deferred<{ success: boolean }>()
    disable.mockReturnValueOnce(pending.promise)
    const { totp, scope, onDisabled } = setup(true)
    totp.password.value = 'password'
    totp.code.value = '123456'
    const request = totp.disable()
    scope.stop()
    pending.resolve({ success: true })
    await request
    expect(onDisabled).not.toHaveBeenCalled()
    expect(totp.enabled.value).toBe(true)
    expect(totp.password.value).toBe('')
  })

  it('does not publish a late failure after disposal', async () => {
    const pending = deferred<typeof setupValue>()
    prepare.mockReturnValueOnce(pending.promise)
    const { totp, scope } = setup()
    totp.password.value = 'password'
    const request = totp.beginSetup()
    scope.stop()
    pending.reject(new Error('Late failure'))
    await request
    expect(totp.error.value).toBe('')
    expect(totp.busy.value).toBe(false)
  })
})

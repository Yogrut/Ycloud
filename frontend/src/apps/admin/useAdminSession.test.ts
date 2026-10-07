import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AdminApiError, getAdminInfo, loginAdministrator, logoutSession, type AdminInfo } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { useAdminSession } from './useAdminSession'

vi.mock('../../shared/api/admin', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/admin')>(),
  getAdminInfo: vi.fn(), loginAdministrator: vi.fn(), logoutSession: vi.fn(),
}))
const read = vi.mocked(getAdminInfo)
const login = vi.mocked(loginAdministrator)
const logout = vi.mocked(logoutSession)
const scopes: EffectScope[] = []
const info: AdminInfo = {
  username: 'admin', has_global_web_password: true, shares: [], folder_locks: [],
  max_upload_bytes: 1024, max_archive_bytes: 1024, max_archive_entries: 100,
  upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: 0,
  admin_login_failures: 3, web_login_failures: 5,
  admin_login_block_seconds: 3600, web_login_block_seconds: 3600,
  security_log_retention_days: 7, security_log_max_entries: 5000,
  storage_instances: [], pending_storage_instance: null, local_storage_path: './storage',
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup(refreshStorage = false) {
  const scope = effectScope()
  scopes.push(scope)
  const session = scope.run(() => useAdminSession(refreshStorage))!
  return { session, scope }
}

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('administrator session', () => {
  it('starts and stops storage refresh when soft navigation changes the active section', async () => {
    vi.useFakeTimers()
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false)
    read.mockResolvedValue(info)
    const scope = effectScope()
    scopes.push(scope)
    const storage = ref(false)
    const session = scope.run(() => useAdminSession(storage))!
    session.start()
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledOnce()
    storage.value = true
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledTimes(2)
    storage.value = false
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(600_000)
    expect(read).toHaveBeenCalledTimes(2)
  })
  it('starts once and does not start network work before mounting', async () => {
    vi.useFakeTimers()
    read.mockResolvedValue(info)
    const { session } = setup()
    expect(session.info.value).toBeUndefined()
    expect(session.loading.value).toBe(true)
    expect(session.password.value).toBe('')
    expect(read).not.toHaveBeenCalled()
    session.start()
    session.start()
    await vi.advanceTimersByTimeAsync(0)
    expect(read).toHaveBeenCalledOnce()
    expect(session.info.value).toEqual(info)
    expect(session.loading.value).toBe(false)
    expect(session.requiresLogin.value).toBe(false)
    await vi.advanceTimersByTimeAsync(600_000)
    expect(read).toHaveBeenCalledOnce()
  })

  it.each([401, 403])('requires a new login and discards administrator info on a %s read', async status => {
    read.mockResolvedValueOnce(info).mockRejectedValueOnce(new AdminApiError('Unauthorized', status))
    const { session } = setup()
    await session.load()
    await session.load(true)
    expect(session.info.value).toBeUndefined()
    expect(session.requiresLogin.value).toBe(true)
    expect(session.loadError.value).toBe('')
    expect(session.loading.value).toBe(false)
  })

  it('separates ordinary read failures from login failures and retains known configuration', async () => {
    read.mockResolvedValueOnce(info).mockRejectedValueOnce(new Error('Read unavailable')).mockResolvedValueOnce(info)
    const { session } = setup()
    session.loginError.value = 'Login error'
    await session.load()
    await session.load(true)
    expect(session.info.value).toEqual(info)
    expect(session.requiresLogin.value).toBe(false)
    expect(session.loadError.value).toBe('Read unavailable')
    expect(session.loginError.value).toBe('Login error')
    await session.load(true)
    expect(session.loadError.value).toBe('')
  })

  it.each(['success', 'error'] as const)('ignores stale read %s even if the transport ignores cancellation', async outcome => {
    const old = deferred<AdminInfo>()
    const latest = deferred<AdminInfo>()
    read.mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise)
    const { session } = setup()
    const oldLoad = session.load()
    const oldSignal = read.mock.calls[0]![0]!
    const currentLoad = session.load(true)
    expect(oldSignal.aborted).toBe(true)
    latest.resolve({ ...info, username: 'new-admin' })
    await currentLoad
    if (outcome === 'success') old.resolve(info)
    else old.reject(new AdminApiError('Old rejection', 401))
    await oldLoad
    expect(session.info.value?.username).toBe('new-admin')
    expect(session.requiresLogin.value).toBe(false)
    expect(session.loadError.value).toBe('')
    expect(session.loading.value).toBe(false)
  })

  it('does not let an obsolete read end the latest loading state', async () => {
    const old = deferred<AdminInfo>()
    const latest = deferred<AdminInfo>()
    read.mockReturnValueOnce(old.promise).mockReturnValueOnce(latest.promise)
    const { session } = setup()
    const oldLoad = session.load()
    const latestLoad = session.load()
    old.resolve(info)
    await oldLoad
    expect(session.loading.value).toBe(true)
    expect(session.info.value).toBeUndefined()
    latest.resolve(info)
    await latestLoad
    expect(session.loading.value).toBe(false)
  })

  it('keeps a confirmed write notice independent of the subsequent read failure', async () => {
    const pending = deferred<AdminInfo>()
    read.mockResolvedValueOnce(info).mockReturnValueOnce(pending.promise)
    const { session } = setup()
    await session.load()
    session.showNotice('Settings saved')
    expect(session.notice.value).toBe('Settings saved')
    expect(session.noticeRevision.value).toBe(1)
    expect(session.loading.value).toBe(false)
    expect(session.info.value).toEqual(info)
    pending.reject(new Error('Refresh failed'))
    await Promise.resolve()
    await Promise.resolve()
    expect(session.loadError.value).toBe('Refresh failed')
    expect(session.notice.value).toBe('Settings saved')
    expect(session.info.value).toEqual(info)
    read.mockResolvedValue(info)
    session.showNotice('Settings saved')
    expect(session.noticeRevision.value).toBe(2)
  })

  it.each([
    { username: '', password: 'password' },
    { username: ' \t', password: 'password' },
    { username: 'admin', password: '' },
  ])('validates required login fields without a request ($username, $password)', async draft => {
    const { session } = setup()
    session.username.value = draft.username
    session.password.value = draft.password
    await session.submitLogin()
    expect(session.loginError.value).toBe('请输入用户名和密码')
    expect(session.loggingIn.value).toBe(false)
    expect(login).not.toHaveBeenCalled()
  })

  it('trims username and proof, preserves password, and waits for administrator info before entering', async () => {
    const pendingLogin = deferred<Awaited<ReturnType<typeof loginAdministrator>>>()
    const pendingRead = deferred<AdminInfo>()
    login.mockReturnValue(pendingLogin.promise)
    read.mockReturnValue(pendingRead.promise)
    const { session } = setup()
    session.requiresLogin.value = true
    session.username.value = ' admin '
    session.password.value = '  current-password  '
    session.totpCode.value = ' proof '
    session.totpRequired.value = true
    const signingIn = session.submitLogin()
    await session.submitLogin()
    expect(login).toHaveBeenCalledOnce()
    expect(login).toHaveBeenCalledWith('admin', '  current-password  ', 'proof')
    pendingLogin.resolve({ success: true, is_admin: true })
    await Promise.resolve()
    await Promise.resolve()
    expect(session.password.value).toBe('')
    expect(session.totpCode.value).toBe('')
    expect(session.totpRequired.value).toBe(false)
    expect(session.requiresLogin.value).toBe(true)
    expect(session.loggingIn.value).toBe(true)
    pendingRead.resolve(info)
    await signingIn
    expect(session.requiresLogin.value).toBe(false)
    expect(session.info.value).toEqual(info)
    expect(session.loggingIn.value).toBe(false)
  })

  it('shows the second-factor challenge only after the server requests it', async () => {
    login.mockResolvedValue({ success: false, is_admin: true, totp_required: true })
    const { session } = setup()
    session.username.value = 'admin'
    session.password.value = 'password'
    await session.submitLogin()
    expect(session.totpRequired.value).toBe(true)
    expect(session.password.value).toBe('password')
    expect(session.loginError.value).toBe('')
    expect(read).not.toHaveBeenCalled()
    session.totpCode.value = '123456'
    session.loginError.value = 'Invalid proof'
    session.resetTotpChallenge()
    expect(session.totpRequired.value).toBe(false)
    expect(session.totpCode.value).toBe('')
    expect(session.loginError.value).toBe('')
    expect(session.password.value).toBe('password')
  })

  it('does not clear an ordinary login error when no second-factor challenge exists', () => {
    const { session } = setup()
    session.loginError.value = 'Login rejected'
    session.resetTotpChallenge()
    expect(session.loginError.value).toBe('Login rejected')
  })

  it('retains rejected credentials for explicit retry without a configuration read', async () => {
    login.mockResolvedValueOnce({ success: false, is_admin: false, message: 'Invalid password' })
      .mockResolvedValueOnce({ success: true, is_admin: true })
    read.mockResolvedValue(info)
    const { session } = setup()
    session.username.value = 'admin'
    session.password.value = 'password'
    await session.submitLogin()
    expect(session.loginError.value).toBe('Invalid password')
    expect(session.password.value).toBe('password')
    expect(login).toHaveBeenCalledOnce()
    expect(read).not.toHaveBeenCalled()
    await session.submitLogin()
    expect(login).toHaveBeenCalledTimes(2)
    expect(read).toHaveBeenCalledOnce()
    expect(session.loginError.value).toBe('')
  })

  it('logs out an ordinary user instead of requesting administrator information', async () => {
    login.mockResolvedValue({ success: true, is_admin: false })
    logout.mockResolvedValue({ success: true })
    const { session } = setup()
    session.requiresLogin.value = true
    session.username.value = 'user'
    session.password.value = 'password'
    await session.submitLogin()
    expect(logout).toHaveBeenCalledOnce()
    expect(session.loginError.value).toBe('用户账号不能进入管理后台')
    expect(session.requiresLogin.value).toBe(true)
    expect(read).not.toHaveBeenCalled()
  })

  it('reports logout failure without opening administrator content or automatically retrying', async () => {
    login.mockResolvedValue({ success: true, is_admin: false })
    logout.mockRejectedValue(new Error('Logout unavailable'))
    const { session } = setup()
    session.username.value = 'user'
    session.password.value = 'password'
    await session.submitLogin()
    expect(session.loginError.value).toBe('Logout unavailable')
    expect(session.info.value).toBeUndefined()
    expect(logout).toHaveBeenCalledOnce()
    expect(read).not.toHaveBeenCalled()
  })

  it('refreshes storage every five minutes without replacing an unfinished read', async () => {
    vi.useFakeTimers()
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false)
    const pending = deferred<AdminInfo>()
    read.mockResolvedValueOnce(info).mockReturnValueOnce(pending.promise).mockResolvedValue(info)
    const { session } = setup(true)
    session.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(299_999)
    expect(read).toHaveBeenCalledOnce()
    await vi.advanceTimersByTimeAsync(1)
    expect(read).toHaveBeenCalledTimes(2)
    expect(session.loading.value).toBe(false)
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledTimes(2)
    expect(read.mock.calls[1]![0]!.aborted).toBe(false)
    pending.resolve(info)
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledTimes(3)
  })

  it('skips storage refresh while hidden, signed out, or logging in', async () => {
    vi.useFakeTimers()
    const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    read.mockResolvedValue(info)
    const { session } = setup(true)
    session.start()
    await vi.advanceTimersByTimeAsync(0)
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledOnce()
    hidden.mockReturnValue(false)
    session.requiresLogin.value = true
    await vi.advanceTimersByTimeAsync(300_000)
    session.requiresLogin.value = false
    session.loggingIn.value = true
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledOnce()
    session.loggingIn.value = false
    await vi.advanceTimersByTimeAsync(300_000)
    expect(read).toHaveBeenCalledTimes(2)
  })

  it('schedules one delayed redirect and cancels it when the page is disposed', async () => {
    vi.useFakeTimers()
    const replace = vi.spyOn(window.location, 'replace').mockImplementation(() => {})
    const { session, scope } = setup()
    session.sessionExpired()
    session.sessionExpired()
    await vi.advanceTimersByTimeAsync(699)
    expect(replace).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(1)
    expect(replace).toHaveBeenCalledExactlyOnceWith('/browse')
    const other = setup()
    other.session.sessionExpired()
    other.scope.stop()
    scope.stop()
    await vi.advanceTimersByTimeAsync(700)
    expect(replace).toHaveBeenCalledOnce()
  })

  it.each(['success', 'error'] as const)('aborts reads and drops their late %s after disposal', async outcome => {
    const pending = deferred<AdminInfo>()
    read.mockReturnValue(pending.promise)
    const { session, scope } = setup(true)
    const reading = session.load()
    const signal = read.mock.calls[0]![0]!
    scope.stop()
    expect(signal.aborted).toBe(true)
    if (outcome === 'success') pending.resolve(info)
    else pending.reject(new AdminApiError('Late rejection', 401))
    await reading
    expect(session.info.value).toBeUndefined()
    expect(session.requiresLogin.value).toBe(false)
    expect(session.loadError.value).toBe('')
    await session.load()
    session.start()
    session.showNotice('Late notice')
    expect(read).toHaveBeenCalledOnce()
    expect(session.notice.value).toBe('')
  })

  it.each(['success', 'error', 'challenge', 'user'] as const)('clears local secrets and ignores a late login %s without follow-up requests', async outcome => {
    const pending = deferred<Awaited<ReturnType<typeof loginAdministrator>>>()
    login.mockReturnValue(pending.promise)
    const { session, scope } = setup()
    session.username.value = 'admin'
    session.password.value = 'password'
    session.totpCode.value = '123456'
    const signingIn = session.submitLogin()
    scope.stop()
    expect(session.password.value).toBe('')
    expect(session.totpCode.value).toBe('')
    if (outcome === 'error') pending.reject(new Error('Late login rejection'))
    else pending.resolve({ success: outcome !== 'challenge', is_admin: outcome !== 'user', totp_required: outcome === 'challenge' })
    await signingIn
    expect(session.totpRequired.value).toBe(false)
    expect(session.loginError.value).toBe('')
    expect(session.loggingIn.value).toBe(false)
    expect(read).not.toHaveBeenCalled()
    expect(logout).not.toHaveBeenCalled()
    await session.submitLogin()
    expect(login).toHaveBeenCalledOnce()
  })

  it('stops the storage refresh timer when its scope is disposed', async () => {
    vi.useFakeTimers()
    read.mockResolvedValue(info)
    const { session, scope } = setup(true)
    session.start()
    await vi.advanceTimersByTimeAsync(0)
    scope.stop()
    await vi.advanceTimersByTimeAsync(900_000)
    expect(read).toHaveBeenCalledOnce()
  })
})

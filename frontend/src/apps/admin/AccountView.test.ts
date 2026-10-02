import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import AccountView from './AccountView.vue'
import { STORED_PASSWORD_MASK } from '../../shared/passwordInput'

const info: AdminInfo = {
  username: 'ycloud-admin',
  has_global_web_password: true,
  shares: [],
  folder_locks: [],
  max_upload_bytes: 1024,
  max_archive_bytes: 1024,
  max_archive_entries: 100,
  upload_rate_bytes_per_sec: 0,
  download_rate_bytes_per_sec: 0,
  admin_login_failures: 3,
  web_login_failures: 5,
  admin_login_block_seconds: 3600,
  web_login_block_seconds: 3600,
  security_log_retention_days: 7,
  security_log_max_entries: 5000,
  storage_instances: [{ id: 'primary', name: '本地存储', enabled: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,

  local_storage_path: './storage',
}

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountAccount(host: HTMLElement, currentInfo = info, onSaved?: (message: string) => void, onExpired?: () => void) {
  const app = createApp(AccountView, { info: currentInfo, onSaved, onExpired })
  app.mount(host)
  return app
}

describe('AccountView', () => {
  it.each([
    { index: 0, value: '  renamed-admin  ', body: { username: 'renamed-admin' }, expires: true },
    { index: 1, value: '  new-password  ', body: { password: '  new-password  ' }, expires: true },
    { index: 2, value: '  web-password  ', body: { global_web_password: '  web-password  ' }, expires: false },
  ])('submits only the edited field and expires only for administrator credentials ($index)', async ({ index, value, body, expires }) => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, info, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[index]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.account-credentials input')!
      if (index === 1) expect(input.minLength).toBe(12)
      input.value = value
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(fetchMock).toHaveBeenCalledWith('/api/admin/account', expect.objectContaining({ method: 'PUT', body: JSON.stringify(body) }))
      expect(saved).toHaveBeenCalledWith(expires ? '账户已更新，请重新登录' : '账户设置已更新')
      expect(expired).toHaveBeenCalledTimes(expires ? 1 : 0)
      expect(host.querySelector('.settings-drawer')).toBeNull()
    } finally { app.unmount() }
  })

  it('does not submit an unchanged normalized username', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[0]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[autocomplete="username"]')!
      input.value = ` ${info.username} `
      input.dispatchEvent(new Event('input'))
      await nextTick()
      expect(host.querySelector<HTMLButtonElement>('.account-credentials button[type="submit"]')!.disabled).toBe(true)
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      expect(fetchMock).not.toHaveBeenCalled()
    } finally { app.unmount() }
  })

  it('submits browser password removal only after confirmation without expiring the administrator session', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, info, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[2]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="password"]')!
      input.value = ''
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).not.toHaveBeenCalled()
      host.querySelector<HTMLButtonElement>('.confirmation-actions .btn:not(.secondary)')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ global_web_password: '' })
      expect(saved).toHaveBeenCalledWith('账户设置已更新')
      expect(expired).not.toHaveBeenCalled()
      expect(host.querySelector('.settings-drawer')).toBeNull()
      expect(host.querySelector('.confirmation-dialog')).toBeNull()
    } finally { app.unmount() }
  })

  it('holds a single submitted snapshot while saving and preserves a server warning', async () => {
    let finish!: (response: Response) => void
    const fetchMock = vi.fn().mockImplementation(() => new Promise<Response>(resolve => { finish = resolve }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, info, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[0]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[autocomplete="username"]')!
      input.value = 'renamed-admin'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(host.querySelector<HTMLButtonElement>('.account-credentials .secondary')!.disabled).toBe(true)
      expect(host.querySelector<HTMLButtonElement>('.account-credentials button[type="submit"]')!.disabled).toBe(true)
      input.value = 'later-draft'
      input.dispatchEvent(new Event('input'))
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ username: 'renamed-admin' })
      expect(saved).not.toHaveBeenCalled()
      expect(expired).not.toHaveBeenCalled()
      finish(new Response(JSON.stringify({ success: true, warning: 'Initial credentials cleanup failed' }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(saved).toHaveBeenCalledWith('Initial credentials cleanup failed')
      expect(expired).toHaveBeenCalledTimes(1)
    } finally { app.unmount() }
  })

  it('retains rejected input and waits for an explicit retry before reporting success', async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'Save rejected' } }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, info, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[1]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="password"]')!
      input.value = 'new-password'
      input.dispatchEvent(new Event('input'))
      const form = host.querySelector('form')!
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(input.value).toBe('new-password')
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Save rejected')
      expect(saved).not.toHaveBeenCalled()
      expect(expired).not.toHaveBeenCalled()
      form.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(saved).toHaveBeenCalledWith('账户已更新，请重新登录')
      expect(expired).toHaveBeenCalledTimes(1)
    } finally { app.unmount() }
  })

  it.each([
    { index: 0, value: ' ', message: '管理员用户名不能为空' },
    { index: 1, value: 'short', message: '管理员密码至少需要 12 位' },
    { index: 2, value: 'short', message: '网页访问密码至少需要 8 位' },
  ])('rejects invalid edited input without a request ($index)', async ({ index, value, message }) => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[index]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.account-credentials input')!
      input.value = value
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await nextTick()
      expect(fetchMock).not.toHaveBeenCalled()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain(message)
    } finally { app.unmount() }
  })

  it.each([
    { index: 0, expected: info.username, protectedBrowser: true },
    { index: 1, expected: STORED_PASSWORD_MASK, protectedBrowser: true },
    { index: 2, expected: STORED_PASSWORD_MASK, protectedBrowser: true },
    { index: 2, expected: '', protectedBrowser: false },
  ])('discards cancelled drafts and restores the existing value ($index, $protectedBrowser)', async ({ index, expected, protectedBrowser }) => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, { ...info, has_global_web_password: protectedBrowser })
    try {
      const edit = host.querySelectorAll<HTMLButtonElement>('.setting-row button')[index]!
      edit.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('.account-credentials input')!
      input.value = 'changed-value'
      input.dispatchEvent(new Event('input'))
      host.querySelector<HTMLButtonElement>('.account-credentials .secondary')!.click()
      await nextTick()
      edit.click()
      await nextTick()
      expect(host.querySelector<HTMLInputElement>('.account-credentials input')!.value).toBe(expected)
      expect(host.querySelector<HTMLButtonElement>('.account-credentials button[type="submit"]')!.disabled).toBe(true)
      expect(fetchMock).not.toHaveBeenCalled()
    } finally { app.unmount() }
  })

  it('validates the mask as new input when no web password exists', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, { ...info, has_global_web_password: false })
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[2]!.click()
      await nextTick()
      const input = host.querySelector<HTMLInputElement>('input[type="password"]')!
      input.value = STORED_PASSWORD_MASK
      input.dispatchEvent(new Event('input'))
      host.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).not.toHaveBeenCalled()
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('网页访问密码至少需要 8 位')
      expect(input.value).toBe(STORED_PASSWORD_MASK)
    } finally { app.unmount() }
  })

  it('keeps recovery codes visible until explicit close and expires the session only then', async () => {
    let finishEnable!: (response: Response) => void
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ secret: 'TESTSECRET234567', provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' }), { headers: { 'Content-Type': 'application/json' } }))
      .mockImplementationOnce(() => new Promise<Response>(resolve => { finishEnable = resolve }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, info, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[3]!.click()
      await nextTick()
      const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = '  current-password  '
      password.dispatchEvent(new Event('input'))
      await nextTick()
      host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      const code = host.querySelector<HTMLInputElement>('input[autocomplete="one-time-code"]')!
      code.value = ' 123456 '
      code.dispatchEvent(new Event('input'))
      await nextTick()
      const confirm = host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!
      confirm.click()
      confirm.click()
      expect(fetchMock).toHaveBeenCalledTimes(2)
      expect(JSON.parse(fetchMock.mock.calls[0]![1].body)).toEqual({ current_password: '  current-password  ' })
      expect(JSON.parse(fetchMock.mock.calls[1]![1].body)).toEqual({ current_password: '  current-password  ', secret: 'TESTSECRET234567', code: '123456' })
      expect(saved).not.toHaveBeenCalled()
      expect(expired).not.toHaveBeenCalled()
      finishEnable(new Response(JSON.stringify({ success: true, recovery_codes: ['first-code', 'second-code'] }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect([...host.querySelectorAll('.totp-section code')].map(item => item.textContent)).toEqual(['first-code', 'second-code'])
      expect(host.querySelector('input[autocomplete="current-password"]')).toBeNull()
      expect(host.querySelector('.totp-secret-field')).toBeNull()
      expect(expired).not.toHaveBeenCalled()
      host.querySelector<HTMLButtonElement>('.totp-section .modal-actions .btn')!.click()
      await nextTick()
      expect(saved).toHaveBeenCalledWith('两步验证已启用，请重新登录')
      expect(expired).toHaveBeenCalledTimes(1)
      expect(host.querySelector('.settings-drawer')).toBeNull()
    } finally { app.unmount() }
  })

  it('expires the session immediately after confirmed disable, not after a rejected proof', async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'forbidden', message: 'Invalid proof' } }), { status: 403, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, { ...info, admin_totp_enabled: true }, saved, expired)
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[3]!.click()
      await nextTick()
      const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = 'current-password'
      password.dispatchEvent(new Event('input'))
      const code = host.querySelector<HTMLInputElement>('input[autocomplete="one-time-code"]')!
      code.value = ' recovery-code '
      code.dispatchEvent(new Event('input'))
      await nextTick()
      host.querySelector<HTMLButtonElement>('.admin-save-row .btn.danger')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(document.querySelector('.app-toast.error')?.textContent).toContain('Invalid proof')
      expect(password.value).toBe('current-password')
      expect(expired).not.toHaveBeenCalled()
      expect(saved).not.toHaveBeenCalled()
      expect(fetchMock).toHaveBeenCalledTimes(1)
      host.querySelector<HTMLButtonElement>('.admin-save-row .btn.danger')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(fetchMock).toHaveBeenLastCalledWith('/api/admin/account/totp', expect.objectContaining({ method: 'DELETE', body: JSON.stringify({ current_password: 'current-password', code: 'recovery-code' }) }))
      expect(saved).toHaveBeenCalledWith('两步验证已停用，请重新登录')
      expect(expired).toHaveBeenCalledTimes(1)
      expect(host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!.value).toBe('')
      expect(host.querySelector('input[autocomplete="one-time-code"]')).toBeNull()
    } finally { app.unmount() }
  })

  it('discards a cancelled setup and its proofs when the editor is reopened', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ secret: 'TESTSECRET234567', provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)
    try {
      const edit = host.querySelectorAll<HTMLButtonElement>('.setting-row button')[3]!
      edit.click()
      await nextTick()
      const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = 'current-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()
      host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!.click()
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(host.querySelector('.totp-secret-field')).not.toBeNull()
      host.querySelector<HTMLButtonElement>('.admin-save-row .secondary')!.click()
      await nextTick()
      edit.click()
      await nextTick()
      expect(host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!.value).toBe('')
      expect(host.querySelector('input[autocomplete="one-time-code"]')).toBeNull()
      expect(host.querySelector('.totp-secret-field')).toBeNull()
      expect(fetchMock).toHaveBeenCalledTimes(1)
    } finally { app.unmount() }
  })

  it.each(['setup', 'enable', 'disable'] as const)('does not cancel a submitted %s request or notify after unmounting', async operation => {
    let finish!: (response: Response) => void
    let signal!: AbortSignal
    const fetchMock = vi.fn().mockImplementation((_url: string, options: RequestInit) => {
      signal = options.signal!
      return new Promise<Response>(resolve => { finish = resolve })
    })
    if (operation === 'enable') fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ secret: 'TESTSECRET234567', provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const saved = vi.fn()
    const expired = vi.fn()
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host, { ...info, admin_totp_enabled: operation === 'disable' }, saved, expired)
    let mounted = true
    try {
      host.querySelectorAll<HTMLButtonElement>('.setting-row button')[3]!.click()
      await nextTick()
      const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = 'current-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()
      if (operation === 'enable') {
        host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!.click()
        await new Promise(resolve => setTimeout(resolve, 0))
      }
      if (operation !== 'setup') {
        const code = host.querySelector<HTMLInputElement>('input[autocomplete="one-time-code"]')!
        code.value = '123456'
        code.dispatchEvent(new Event('input'))
        await nextTick()
      }
      host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!.click()
      expect(fetchMock).toHaveBeenCalledTimes(operation === 'enable' ? 2 : 1)
      app.unmount()
      mounted = false
      expect(signal.aborted).toBe(false)
      finish(new Response(JSON.stringify(operation === 'setup' ? { secret: 'TESTSECRET234567', provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' } : { success: true, recovery_codes: ['recovery-code'] }), { headers: { 'Content-Type': 'application/json' } }))
      await new Promise(resolve => setTimeout(resolve, 0))
      expect(signal.aborted).toBe(false)
      expect(saved).not.toHaveBeenCalled()
      expect(expired).not.toHaveBeenCalled()
      expect(fetchMock).toHaveBeenCalledTimes(operation === 'enable' ? 2 : 1)
    } finally { if (mounted) app.unmount() }
  })

  it('keeps the generated secret disabled while providing an explicit copy action', async () => {
    const secret = 'TESTSECRET234567'
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ secret, provisioning_uri: 'otpauth://test', qr_svg: '<svg/>' }), { headers: { 'Content-Type': 'application/json' } })))
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)
    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[3]!.click()
    await nextTick()
    const password = host.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
    password.value = 'test-password'
    password.dispatchEvent(new Event('input'))
    await nextTick()
    host.querySelector<HTMLButtonElement>('.admin-save-row .btn:not(.secondary)')!.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    const field = host.querySelector<HTMLInputElement>('.totp-secret-field input')!
    expect(field.disabled).toBe(true)
    field.focus()
    expect(document.activeElement).not.toBe(field)
    const copy = host.querySelector<HTMLButtonElement>('.readonly-copy-field button')!
    expect(copy.previousElementSibling).toBe(field)
    expect(copy.getAttribute('aria-label')).toBe('复制密钥')
    expect(copy.textContent).toBe('')
    expect(copy.querySelector('svg')?.getAttribute('data-icon')).toBe('copy')
    copy.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(writeText).toHaveBeenCalledWith(secret)
    expect(document.querySelector('.app-toast.success')?.textContent).toContain('密钥已复制')
    writeText.mockRejectedValueOnce(new Error('Clipboard unavailable'))
    copy.click()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(document.querySelector('.app-toast.error')?.textContent).toContain('无法复制')
    app.unmount()
  })
  it('contains administrator credentials and account-bound two-step verification', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)
    await nextTick()

    expect(host.querySelectorAll('.admin-pane')).toHaveLength(1)
    expect(host.querySelectorAll('.setting-row')).toHaveLength(5)
    expect(host.querySelector('.settings-drawer')).toBeNull()
    expect(host.textContent).toContain('管理员设置')
    expect(host.textContent).toContain('两步验证')
    expect(host.textContent).not.toContain('登录保护')
    expect(host.textContent).not.toContain('保存登录限制')
    app.unmount()
  })

  it('does not submit masked passwords when only the username changes', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountAccount(host)

    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[0]!.click()
    await nextTick()
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

    host.querySelectorAll<HTMLButtonElement>('.setting-row button')[2]!.click()
    await nextTick()
    const passwords = host.querySelectorAll<HTMLInputElement>('input[type="password"]')
    const webPassword = passwords[0]
    if (!webPassword) throw new Error('web password input not found')
    webPassword.value = ''
    webPassword.dispatchEvent(new Event('input'))
    ;(host.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { cancelable: true }))
    await nextTick()

    expect(fetchMock).not.toHaveBeenCalled()
    expect(host.textContent).toContain('确认移除首页密码')
    expect(host.querySelector('.confirmation-warning')?.textContent).toContain('文件夹锁仍然有效')
    expect(Array.from(host.querySelectorAll('.confirmation-actions button'), button => button.textContent)).toEqual(['取消', '确认'])
    host.querySelector<HTMLButtonElement>('.confirmation-actions .secondary')!.click()
    await nextTick()
    expect(host.querySelector('.confirmation-dialog')).toBeNull()
    expect(fetchMock).not.toHaveBeenCalled()
    app.unmount()
  })
})

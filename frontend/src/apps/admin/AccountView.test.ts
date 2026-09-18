import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AdminInfo } from '../../shared/api/admin'
import AccountView from './AccountView.vue'

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
  storage_instances: [{ id: 'primary', name: '本地存储', is_default: true, ready: true, backend: { type: 'local', path: './storage', capacity_limit_bytes: null }, usage_bytes: 0, reserved_bytes: 0 }],
  pending_storage_instance: null,
  default_storage_id: 'primary',
  local_storage_path: './storage',
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

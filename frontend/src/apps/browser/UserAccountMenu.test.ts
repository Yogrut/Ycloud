import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import UserAccountMenu from './UserAccountMenu.vue'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })
const capabilities = { download: true, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false }
const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } })
const settle = async () => { await new Promise(resolve => setTimeout(resolve, 0)); await nextTick() }

function mount() {
  const signedIn = vi.fn()
  const signOut = vi.fn()
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(UserAccountMenu, { storageName: 'Documents', capabilities, onSignedIn: signedIn, onSignOut: signOut })
  app.mount(host)
  return { app, host, signedIn, signOut }
}

describe('UserAccountMenu', () => {
  it('shows focus rings only after Tab and clears them on pointer interaction', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(() => Promise.resolve(json({ logged_in: true, is_admin: false, username: 'reader' }))))
    const { app, host, signOut } = mount()
    await settle()
    const trigger = host.querySelector<HTMLButtonElement>('button')!
    trigger.dispatchEvent(new Event('pointerdown', { bubbles: true }))
    trigger.click()
    await settle()
    const signOutButton = document.querySelector<HTMLButtonElement>('.user-account-signout')!
    expect(document.activeElement).toBe(signOutButton)
    expect(signOutButton.classList.contains('keyboard-focus')).toBe(false)
    for (const key of ['Enter', ' ']) {
      signOutButton.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }))
      await nextTick()
      expect(signOutButton.classList.contains('keyboard-focus')).toBe(false)
    }
    signOutButton.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true }))
    await nextTick()
    expect(signOutButton.classList.contains('keyboard-focus')).toBe(true)
    signOutButton.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await nextTick()
    expect(document.activeElement).toBe(trigger)
    expect(trigger.classList.contains('keyboard-focus')).toBe(true)
    trigger.click()
    await settle()
    const reopenedSignOut = document.querySelector<HTMLButtonElement>('.user-account-signout')!
    expect(document.activeElement).toBe(reopenedSignOut)
    expect(reopenedSignOut.classList.contains('keyboard-focus')).toBe(true)
    reopenedSignOut.dispatchEvent(new Event('pointerdown', { bubbles: true }))
    await nextTick()
    expect(trigger.classList.contains('keyboard-focus')).toBe(false)
    expect(reopenedSignOut.classList.contains('keyboard-focus')).toBe(false)
    reopenedSignOut.click()
    expect(signOut).toHaveBeenCalledTimes(1)
    app.unmount()
  })

  it('shows user information and effective permissions when its icon is clicked', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(() => Promise.resolve(json({ logged_in: true, is_admin: false, username: 'reader' }))))
    const { app, host } = mount()
    await settle()
    expect(host.querySelector('button')?.textContent?.trim()).toBe('')
    host.querySelector<HTMLButtonElement>('button')!.click()
    await settle()
    const dialog = document.querySelector('[role="dialog"]')!
    expect(dialog.getAttribute('aria-label')).toBe('用户信息')
    expect(dialog.textContent).toContain('reader')
    expect(dialog.classList.contains('user-account-popover')).toBe(true)
    expect(document.querySelector('.overlay')).toBeNull()
    expect(dialog.textContent).toContain('普通用户')
    expect(dialog.textContent).toContain('Documents')
    expect(dialog.querySelector('dd:last-child')?.textContent).toBe('下载')
    expect(dialog.querySelector('input[type="password"]')).toBeNull()
    document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }))
    await settle()
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    app.unmount()
  })

  it.each(['pointer', 'enter', 'tab'] as const)('restores focus after %s login with a ring only in Tab mode', async (mode) => {
    let authenticated = false
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url === '/api/user/login') { authenticated = true; return Promise.resolve(json({ success: true, is_admin: false })) }
      return Promise.resolve(json({ logged_in: authenticated, is_admin: false, username: authenticated ? 'reader' : null }))
    })
    vi.stubGlobal('fetch', fetchMock)
    const { app, host, signedIn } = mount()
    await settle()
    host.querySelector<HTMLButtonElement>('button')!.click()
    await settle()
    const inputs = document.querySelectorAll<HTMLInputElement>('.user-account-modal input')
    inputs[0]!.value = 'reader'
    inputs[0]!.dispatchEvent(new Event('input'))
    inputs[1]!.value = 'reader-password'
    inputs[1]!.dispatchEvent(new Event('input'))
    // Start with keyboard navigation, then verify that a mouse submission
    // clears it while Enter alone does not enable it.
    if (mode !== 'enter') document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab' }))
    if (mode === 'pointer') document.querySelector('button[type="submit"]')!.dispatchEvent(new Event('pointerdown', { bubbles: true }))
    if (mode === 'enter') inputs[1]!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await settle()
    expect(fetchMock).toHaveBeenCalledWith('/api/user/login', expect.objectContaining({ method: 'POST', body: JSON.stringify({ username: 'reader', password: 'reader-password' }) }))
    expect(signedIn).toHaveBeenCalledTimes(1)
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    expect(host.querySelector('button')?.getAttribute('aria-label')).toBe('用户信息')
    expect(host.querySelector('button')?.textContent?.trim()).toBe('')
    expect(document.activeElement).toBe(host.querySelector('button'))
    expect(host.querySelector('button')?.classList.contains('keyboard-focus')).toBe(mode === 'tab')
    app.unmount()
  })

  it('keeps an invalid login in the form and does not expose an administrator as a user', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation((url: string) => Promise.resolve(json(url === '/api/me'
      ? { logged_in: true, is_admin: true, username: 'admin' }
      : { success: false, is_admin: false, message: '用户名或密码错误' }))))
    const { app, host, signedIn } = mount()
    await settle()
    expect(host.querySelector('button')?.getAttribute('aria-label')).toBe('用户登录')
    expect(host.querySelector('button')?.textContent?.trim()).toBe('')
    host.querySelector<HTMLButtonElement>('button')!.click()
    await settle()
    for (const input of document.querySelectorAll<HTMLInputElement>('input')) {
      input.value = 'invalid'
      input.dispatchEvent(new Event('input'))
    }
    document.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await settle()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('用户名或密码错误')
    expect(signedIn).not.toHaveBeenCalled()
    app.unmount()
  })
})

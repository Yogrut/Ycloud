import toastSource from './AppToast.vue?raw'
import { createApp } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AppToast from './AppToast.vue'

afterEach(() => { vi.useRealTimers(); document.body.replaceChildren() })

function mountToast(host: HTMLElement, kind: 'error' | 'success', onClose: () => void) {
  const app = createApp(AppToast, { message: '测试结果', kind, onClose })
  app.mount(host)
  return app
}

describe('AppToast', () => {
  it.each(['error', 'success'] as const)('shows %s above the page without moving focus and allows dismissal', kind => {
    vi.useFakeTimers()
    const opener = document.createElement('button')
    const host = document.createElement('div')
    document.body.append(opener, host)
    opener.focus()
    const close = vi.fn()
    const app = mountToast(host, kind, close)
    const toast = document.querySelector('.app-toast')!
    expect(toast.parentElement).toBe(document.body)
    expect(toast.getAttribute('role')).toBe(kind === 'error' ? 'alert' : 'status')
    expect(toast.querySelector('.app-toast-status')?.getAttribute('data-icon')).toBe(kind === 'error' ? 'status-error' : 'status-success')
    expect(toast.querySelector('.app-toast-status')?.getAttribute('data-weight')).toBe('duotone')
    const closeIcon = toast.querySelector('button svg')!
    expect(closeIcon.getAttribute('data-weight')).toBe('regular')
    expect(closeIcon.querySelector('path[opacity]')).toBeNull()
    expect(document.activeElement).toBe(opener)
    toast.querySelector<HTMLButtonElement>('button')!.click()
    expect(close).toHaveBeenCalledTimes(1)
    app.unmount()
    vi.advanceTimersByTime(6000)
    expect(close).toHaveBeenCalledTimes(1)
  })

  it('automatically dismisses after six seconds', () => {
    vi.useFakeTimers()
    const close = vi.fn()
    const app = mountToast(document.createElement('div'), 'success', close)
    vi.advanceTimersByTime(5999)
    expect(close).not.toHaveBeenCalled()
    vi.advanceTimersByTime(1)
    expect(close).toHaveBeenCalledTimes(1)
    app.unmount()
  })

  it('centres semantic red and green notifications above drawers in both themes', () => {
    const source = toastSource
    expect(source).toContain('position: fixed; z-index: 200;')
    expect(source).toContain('left: 50%; transform: translateX(-50%);')
    expect(source).toContain('.app-toast.error { color: #e05267; background: #fff1f2;')
    expect(source).toContain('.app-toast.success { color: #2da66e; background: #effaf3;')
    expect(source).toContain(':global(:root[data-theme="dark"] .app-toast.error)')
    expect(source).toContain(':global(:root[data-theme="dark"] .app-toast.success)')
    expect(source).toContain('color: var(--muted-2); background: transparent;')
  })
})

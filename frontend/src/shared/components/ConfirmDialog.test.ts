import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import ConfirmDialog from './ConfirmDialog.vue'

afterEach(() => { document.body.replaceChildren(); document.body.style.overflow = '' })

function mountDialog(host: HTMLElement, options: {busy?: boolean; onClose: () => void; onConfirm?: () => void}) {
  const app = createApp(ConfirmDialog, {title:'删除', message:'是否继续？', target:'example', ...options})
  app.mount(host)
  return app
}

describe('ConfirmDialog', () => {
  it('focuses cancel, traps focus, supports Escape and restores focus on unmount', async () => {
    const trigger = document.createElement('button'); document.body.append(trigger); trigger.focus()
    const host = document.createElement('div'); document.body.append(host)
    const close = vi.fn()
    const app = mountDialog(host, {onClose:close})
    await nextTick(); await nextTick()
    expect(document.activeElement).toBe(host.querySelector('.secondary'))
    expect(document.body.style.overflow).toBe('hidden')
    const panel = host.querySelector<HTMLElement>('[role="dialog"]')!
    expect(panel.getAttribute('aria-describedby')).toBeTruthy()
    const buttons = host.querySelectorAll<HTMLButtonElement>('button')
    buttons[2]!.focus()
    buttons[2]!.dispatchEvent(new KeyboardEvent('keydown',{key:'Tab',bubbles:true,cancelable:true}))
    expect(document.activeElement).toBe(buttons[0])
    buttons[0]!.dispatchEvent(new KeyboardEvent('keydown',{key:'Tab',shiftKey:true,bubbles:true,cancelable:true}))
    expect(document.activeElement).toBe(buttons[2])
    panel.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))
    expect(close).toHaveBeenCalledTimes(1)
    app.unmount()
    expect(document.activeElement).toBe(trigger)
    expect(document.body.style.overflow).toBe('')
  })

  it('blocks backdrop dismissal, Escape and confirmation while busy', async () => {
    const host = document.createElement('div'); document.body.append(host)
    const close = vi.fn(); const confirm = vi.fn()
    const app = mountDialog(host, {busy:true,onClose:close,onConfirm:confirm})
    await nextTick()
    host.querySelector<HTMLElement>('.overlay')!.click()
    host.querySelector('[role="dialog"]')!.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))
    host.querySelectorAll<HTMLButtonElement>('button').forEach(button => {expect(button.disabled).toBe(true); button.click()})
    expect(close).not.toHaveBeenCalled(); expect(confirm).not.toHaveBeenCalled()
    app.unmount()
  })
})

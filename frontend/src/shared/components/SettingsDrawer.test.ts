import { createApp, h, nextTick, ref, type VNode } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import SettingsDrawer from './SettingsDrawer.vue'

afterEach(() => { document.body.replaceChildren(); document.body.style.overflow = '' })

function mountHarness(host: HTMLElement, render: () => VNode) {
  const app = createApp({ setup: () => render })
  app.mount(host)
  return app
}

describe('SettingsDrawer', () => {
  it('focuses the editor, traps Tab, closes with Escape and restores focus and scrolling', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const open = ref(false)
    const app = mountHarness(host, () => h('div', [h('button', { id: 'opener', onClick: () => { open.value = true } }, 'Open'), open.value ? h(SettingsDrawer, { title: 'Edit', onClose: () => { open.value = false } }, () => h('input')) : null]))
    const opener = host.querySelector<HTMLButtonElement>('#opener')!
    opener.focus(); opener.click()
    await nextTick(); await nextTick()
    const input = host.querySelector('input')!
    expect(document.activeElement).toBe(input)
    expect(document.body.style.overflow).toBe('hidden')
    expect(host.querySelectorAll('.settings-drawer-head button')).toHaveLength(1)
    expect(host.querySelector('.settings-drawer-head button')?.getAttribute('aria-label')).toBe('关闭')
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }))
    expect(document.activeElement).toBe(host.querySelector('.settings-drawer-head button'))
    document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    await nextTick()
    expect(host.querySelector('[role="dialog"]')).toBeNull()
    expect(document.activeElement).toBe(opener)
    expect(document.body.style.overflow).toBe('')
    app.unmount()
  })

  it('does not dismiss during a save or consume Escape handled by a nested control', async () => {
    const host = document.createElement('div'); document.body.append(host)
    let closes = 0
    const busy = ref(true)
    const app = mountHarness(host, () => h(SettingsDrawer, { title: 'Edit', busy: busy.value, onClose: () => { closes++ } }, () => h('input')))
    await nextTick()
    host.querySelector('.settings-drawer-backdrop')!.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    host.querySelector('input')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    expect(closes).toBe(0)
    busy.value = false; await nextTick()
    const event = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }); event.preventDefault()
    host.querySelector('input')!.dispatchEvent(event)
    expect(closes).toBe(0)
    app.unmount()
  })
})

import { createApp } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import App from './App.vue'

afterEach(() => {
  sessionStorage.removeItem('ycloud-stay-signed-out')
  document.body.replaceChildren()
})

describe('Ycloud native context menu', () => {
  it('suppresses the browser menu across the page while mounted', () => {
    sessionStorage.setItem('ycloud-stay-signed-out', '1')
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(App)
    app.mount(host)

    const inside = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 })
    host.dispatchEvent(inside)
    expect(inside.defaultPrevented).toBe(true)

    app.unmount()
    const afterUnmount = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 })
    host.dispatchEvent(afterUnmount)
    expect(afterUnmount.defaultPrevented).toBe(false)
  })
})

import { createApp, nextTick } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import AppSwitch from './AppSwitch.vue'

function mountSwitch(disabled = false) {
  const host = document.createElement('div')
  document.body.append(host)
  const update = vi.fn()
  const app = createApp(AppSwitch, {modelValue:false,label:'启用账号',disabled,'onUpdate:modelValue':update})
  app.mount(host)
  return {host, app, update}
}

describe('AppSwitch', () => {
  it('exposes a labelled native switch and emits a boolean without submitting a form', async () => {
    const {host,app,update} = mountSwitch()
    const input = host.querySelector<HTMLInputElement>('input[role="switch"]')!
    expect(input.type).toBe('checkbox')
    expect(input.getAttribute('aria-label')).toBe('启用账号')
    expect(input.getAttribute('aria-checked')).toBe('false')
    input.click(); await nextTick()
    expect(update).toHaveBeenCalledWith(true)
    app.unmount(); host.remove()
  })

  it('cannot change while disabled', () => {
    const {host,app,update} = mountSwitch(true)
    host.querySelector<HTMLInputElement>('input')!.click()
    expect(update).not.toHaveBeenCalled()
    app.unmount(); host.remove()
  })
})

import { createApp, h, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import AppSelect from './AppSelect.vue'

afterEach(() => document.body.replaceChildren())

describe('AppSelect', () => {
  it('opens an accessible listbox and updates the selected value', async () => {
    const value = ref<string | number>('local')
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp({
      setup: () => () => h(AppSelect, {
        modelValue: value.value,
        options: [
          { value: 'local', label: 'Local storage' },
          { value: 'remote', label: 'RustFS' },
        ],
        label: 'Switch storage',
        'onUpdate:modelValue': selected => { value.value = selected },
      }),
    })
    app.mount(host)

    const trigger = host.querySelector<HTMLButtonElement>('.app-select-trigger')!
    expect(trigger.getAttribute('aria-label')).toBe('Switch storage')
    trigger.click()
    await nextTick()
    expect(host.querySelector('[role="listbox"]')).not.toBeNull()
    const remote = [...host.querySelectorAll<HTMLButtonElement>('.app-select-option')]
      .find(option => option.textContent?.trim() === 'RustFS')!
    remote.click()
    await nextTick()

    expect(value.value).toBe('remote')
    expect(trigger.textContent).toContain('RustFS')
    expect(host.querySelector('[role="listbox"]')).toBeNull()
    app.unmount()
  })
})

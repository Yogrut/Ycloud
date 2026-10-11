import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import AppDatePicker from './AppDatePicker.vue'

const apps: ReturnType<typeof createApp>[] = []
function mount(props: Record<string, unknown>): HTMLElement {
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(AppDatePicker, props)
  apps.push(app)
  app.mount(host)
  return host
}
afterEach(() => {
  apps.splice(0).forEach(app => app.unmount())
  document.body.replaceChildren()
})

describe('AppDatePicker', () => {
  it('uses the project calendar instead of a native date input', async () => {
    const value = ref('2026-09-01')
    const host = mount({
      modelValue: value.value,
      label: '日期',
      max: '2026-09-30',
      'onUpdate:modelValue': (next: string) => { value.value = next },
    })
    host.querySelector<HTMLButtonElement>('.date-picker-trigger')!.click()
    await nextTick()
    expect(host.querySelector('input[type="date"]')).toBeNull()
    expect(document.querySelector('[role="dialog"]')).not.toBeNull()
    const day = [...document.querySelectorAll<HTMLButtonElement>('.calendar-grid button')]
      .find(button => button.textContent === '2' && !button.classList.contains('outside'))!
    day.click(); await nextTick()
    expect(value.value).toBe('2026-09-02')
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    expect(document.activeElement).toBe(host.querySelector('.date-picker-trigger'))
  })

  it('limits selectable dates to the requested range', async () => {
    const host = mount({ modelValue: '2026-09-15', label: '开始日期', min: '2026-09-10', max: '2026-09-20' })
    host.querySelector<HTMLButtonElement>('.date-picker-trigger')!.click()
    await nextTick()
    const days = [...document.querySelectorAll<HTMLButtonElement>('.calendar-grid button')]
      .filter(button => !button.classList.contains('outside'))
    expect(days.filter(button => !button.disabled).map(button => button.textContent)).toEqual(
      Array.from({ length: 11 }, (_, index) => String(index + 10)),
    )
    expect(document.querySelector('.date-picker-popover footer')).toBeNull()
  })

  it('teleports the popup outside clipping containers and supports top placement', async () => {
    const host = mount({ modelValue: '2026-09-15', label: '起始日期', placement: 'top' })
    host.querySelector<HTMLButtonElement>('.date-picker-trigger')!.click()
    await nextTick()
    const dialog = document.querySelector<HTMLElement>('.date-picker-popover')!
    expect(host.querySelector('.date-picker-popover')).toBeNull()
    expect(dialog.classList.contains('placement-top')).toBe(true)
    expect(dialog.style.position).toBe('')
    expect(dialog.querySelector('footer')).toBeNull()
  })
})

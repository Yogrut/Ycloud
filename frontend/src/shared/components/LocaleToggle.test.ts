import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import { useLocale } from '../i18n'
import LocaleToggle from './LocaleToggle.vue'

afterEach(() => {
  document.body.replaceChildren()
  useLocale().set('zh-CN')
})

describe('LocaleToggle', () => {
  it('uses a translation icon while preserving the language switch label', async () => {
    const locale = useLocale()
    locale.set('zh-CN')
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(LocaleToggle)
    app.mount(host)

    const button = host.querySelector<HTMLButtonElement>('button')
    expect(button?.getAttribute('aria-label')).toBe('Switch to English')
    expect(button?.querySelector('[data-icon="language"][data-weight="duotone"]')?.tagName.toLowerCase()).toBe('svg')
    expect(button?.textContent?.trim()).toBe('')

    button?.click()
    await nextTick()
    expect(button?.getAttribute('aria-label')).toBe('切换到中文')
    expect(locale.current.value).toBe('en')
    app.unmount()
  })
})

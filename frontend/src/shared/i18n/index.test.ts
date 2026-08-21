import { beforeEach, describe, expect, it } from 'vitest'
import { useLocale } from './index'

describe('locale state', () => {
  beforeEach(() => {
    window.localStorage.clear()
    useLocale().set('zh-CN')
  })

  it('uses Chinese by default and interpolates values', () => {
    const locale = useLocale()
    expect(locale.current.value).toBe('zh-CN')
    expect(locale.t('menu.selected', { count: 3 })).toBe('已选择 3 项')
    expect(document.documentElement.lang).toBe('zh-CN')
  })

  it('persists English and keeps all keys available', () => {
    const locale = useLocale()
    locale.set('en')
    expect(locale.t('menu.selected', { count: 2 })).toBe('2 selected')
    expect(window.localStorage.getItem('ycloud-locale')).toBe('en')
    expect(document.documentElement.lang).toBe('en')
  })
})

import { beforeEach, describe, expect, it } from 'vitest'
import { useTheme } from './useTheme'

beforeEach(() => {
  localStorage.clear()
  delete document.documentElement.dataset.theme
})

describe('theme controller', () => {
  it('uses the light theme by default', () => {
    const theme = useTheme()
    expect(theme.current.value).toBe('light')
    expect(document.documentElement.dataset.theme).toBe('light')
  })

  it('persists the selected theme', () => {
    const theme = useTheme()
    theme.toggle()
    expect(theme.current.value).toBe('dark')
    expect(localStorage.getItem('ycloud-theme')).toBe('dark')
  })
})

import { ref, type Ref } from 'vue'

export type Theme = 'light' | 'dark'

const THEME_KEY = 'ycloud-theme'

function storedTheme(): Theme {
  try {
    const value = localStorage.getItem(THEME_KEY)
    if (value === 'light' || value === 'dark') return value
  } catch {
    // The product default remains available when storage is blocked.
  }
  return 'light'
}

function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme
}

export interface ThemeController {
  current: Ref<Theme>
  toggle: () => void
}

export function useTheme(): ThemeController {
  const current = ref<Theme>(storedTheme())
  applyTheme(current.value)

  function toggle(): void {
    current.value = current.value === 'dark' ? 'light' : 'dark'
    applyTheme(current.value)
    try {
      localStorage.setItem(THEME_KEY, current.value)
    } catch {
      // Theme remains active for the current page.
    }
  }

  return { current, toggle }
}

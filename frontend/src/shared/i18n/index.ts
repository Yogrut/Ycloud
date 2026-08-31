import { computed, readonly, ref } from 'vue'
import { en } from './en'
import { zhCN, type MessageKey } from './zh-CN'

export type Locale = 'zh-CN' | 'en'
export type MessageParams = Record<string, string | number>

const STORAGE_KEY = 'ycloud-locale'
const messages: Record<Locale, Record<MessageKey, string>> = {
  'zh-CN': zhCN,
  en,
}

function storedLocale(): Locale {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY)
    return value === 'en' || value === 'zh-CN' ? value : 'zh-CN'
  } catch {
    return 'zh-CN'
  }
}

const current = ref<Locale>(storedLocale())

function applyLocale(locale: Locale): void {
  current.value = locale
  document.documentElement.lang = locale
  try {
    window.localStorage.setItem(STORAGE_KEY, locale)
  } catch {
    // Language switching remains available when storage is disabled.
  }
}

function t(key: MessageKey, params: MessageParams = {}): string {
  const template = messages[current.value][key]
  return template.replace(/\{(\w+)\}/g, (match, name: string) => (
    Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : match
  ))
}

function toggle(): void {
  applyLocale(current.value === 'zh-CN' ? 'en' : 'zh-CN')
}

function text(chinese: string, english: string): string {
  return current.value === 'en' ? english : chinese
}

applyLocale(current.value)

export function useLocale() {
  return {
    current: readonly(current),
    isEnglish: computed(() => current.value === 'en'),
    set: applyLocale,
    toggle,
    t,
    text,
  }
}

import type { BrowserStorage } from '../../shared/api/browser'

export function isStorageUsable(storage: BrowserStorage | undefined): boolean {
  return storage !== undefined
    && !storage.requires_login
    && storage.enabled !== false
    && storage.ready !== false
}

export function shouldForgetStorage(storage: BrowserStorage | undefined): boolean {
  // A temporary health failure must not erase the account's preference.
  return storage === undefined || storage.enabled === false || storage.requires_login
}

export function rememberedStorage(scope: string): string {
  try {
    return localStorage.getItem(`ycloud:storage:${scope}`) ?? ''
  } catch {
    return ''
  }
}

export function rememberStorage(scope: string, id: string): void {
  try {
    const key = `ycloud:storage:${scope}`
    if (id) localStorage.setItem(key, id)
    else localStorage.removeItem(key)
  } catch {
    // Storage preferences are optional when browser storage is unavailable.
  }
}

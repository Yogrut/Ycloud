import { afterEach, describe, expect, it, vi } from 'vitest'
import type { BrowserStorage } from '../../shared/api/browser'
import { isStorageUsable, rememberedStorage, rememberStorage, shouldForgetStorage } from './storageSelection'

const storage: BrowserStorage = { id: 'primary', name: 'Primary', requires_login: false }

afterEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('storage selection policy', () => {
  it.each([
    { name: 'missing', value: undefined, usable: false, forget: true },
    { name: 'legacy available', value: storage, usable: true, forget: false },
    { name: 'enabled and ready', value: { ...storage, enabled: true, ready: true }, usable: true, forget: false },
    { name: 'temporarily unavailable', value: { ...storage, ready: false }, usable: false, forget: false },
    { name: 'disabled', value: { ...storage, enabled: false }, usable: false, forget: true },
    { name: 'requires login', value: { ...storage, requires_login: true }, usable: false, forget: true },
    { name: 'disabled and unavailable', value: { ...storage, enabled: false, ready: false }, usable: false, forget: true },
  ])('handles $name separately for selection and preference retention', ({ value, usable, forget }) => {
    expect(isStorageUsable(value)).toBe(usable)
    expect(shouldForgetStorage(value)).toBe(forget)
  })

  it('isolates choices by identity and clears only the requested choice', () => {
    rememberStorage('guest', 'first')
    rememberStorage('user:alice', 'second')
    expect(rememberedStorage('guest')).toBe('first')
    expect(rememberedStorage('user:alice')).toBe('second')
    expect(rememberedStorage('user:bob')).toBe('')
    rememberStorage('guest', '')
    expect(rememberedStorage('guest')).toBe('')
    expect(rememberedStorage('user:alice')).toBe('second')
  })

  it('treats blocked browser storage reads as an absent preference', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new DOMException('Blocked', 'SecurityError')
    })
    expect(rememberedStorage('guest')).toBe('')
  })

  it.each(['setItem', 'removeItem'] as const)('tolerates %s failures', method => {
    vi.spyOn(Storage.prototype, method).mockImplementation(() => {
      throw new DOMException('Blocked', 'SecurityError')
    })
    expect(() => rememberStorage('guest', method === 'setItem' ? 'primary' : '')).not.toThrow()
  })
})

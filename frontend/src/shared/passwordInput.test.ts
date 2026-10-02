import { nextTick } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import { selectStoredPassword, STORED_PASSWORD_MASK } from './passwordInput'

describe('stored password input', () => {
  it('selects the placeholder after the Vue update without changing the input', async () => {
    const input = document.createElement('input')
    input.type = 'password'
    input.value = STORED_PASSWORD_MASK
    const select = vi.spyOn(input, 'select')
    input.addEventListener('focus', selectStoredPassword)
    input.dispatchEvent(new FocusEvent('focus'))
    expect(select).not.toHaveBeenCalled()
    await nextTick()
    expect(select).toHaveBeenCalledTimes(1)
    expect(input.value).toBe(STORED_PASSWORD_MASK)
  })

  it.each(['', 'replacement password'])('does not select a real password or empty field: %s', async value => {
    const input = document.createElement('input')
    input.value = value
    const select = vi.spyOn(input, 'select')
    input.addEventListener('focus', selectStoredPassword)
    input.dispatchEvent(new FocusEvent('focus'))
    await nextTick()
    expect(select).not.toHaveBeenCalled()
    expect(input.value).toBe(value)
  })
})

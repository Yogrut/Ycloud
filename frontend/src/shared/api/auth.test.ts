import { afterEach, describe, expect, it, vi } from 'vitest'
import { enterGate, getIdentity } from './auth'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('authentication API', () => {
  it('reads the current identity with same-origin credentials', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ logged_in: true }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await expect(getIdentity()).resolves.toEqual({ logged_in: true })
    expect(fetchMock).toHaveBeenCalledWith('/api/me', { credentials: 'same-origin' })
  })

  it('preserves the server error when gate authentication fails', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ message: '尝试次数过多' }), {
        status: 429,
        headers: { 'Content-Type': 'application/json' },
      }),
    ))

    await expect(enterGate('wrong')).rejects.toThrow('尝试次数过多')
  })
})

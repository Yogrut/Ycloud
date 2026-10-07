import { afterEach, describe, expect, it, vi } from 'vitest'
import { enterGate, getIdentity, logoutSession } from './auth'
import { logout } from './browser'
import { logoutSession as adminLogout } from './admin'

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe('authentication API', () => {
  it('uses one checked logout request for file and admin callers', async () => {
    expect(logout).toBe(logoutSession)
    expect(adminLogout).toBe(logoutSession)
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true })))
    vi.stubGlobal('fetch', fetchMock)
    await expect(logoutSession()).resolves.toEqual({ success: true })
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock).toHaveBeenCalledWith('/api/logout', expect.objectContaining({
      method: 'POST', credentials: 'same-origin', signal: expect.any(AbortSignal),
    }))
  })

  it.each([
    [200, { success: false }], [200, {}], [403, { success: true }],
    [500, { error: { message: 'Service unavailable' } }],
  ])('does not accept HTTP %s without successful logout confirmation', async (status, body) => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(body), { status })))
    await expect(logoutSession()).rejects.toMatchObject({ status })
  })

  it('does not treat a lost response or invalid success body as successful logout', async () => {
    const fetchMock = vi.fn().mockRejectedValueOnce(new TypeError('Disconnected'))
      .mockResolvedValueOnce(new Response('<html>proxy error</html>'))
    vi.stubGlobal('fetch', fetchMock)
    await expect(logoutSession()).rejects.toMatchObject({ code: 'operation_result_unknown' })
    await expect(logoutSession()).rejects.toMatchObject({ code: 'operation_result_unknown' })
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })

  it('bounds a stalled logout wait and does not retry automatically', async () => {
    vi.useFakeTimers()
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => undefined))
    vi.stubGlobal('fetch', fetchMock)
    const result = expect(logoutSession()).rejects.toMatchObject({ code: 'operation_result_unknown' })
    await vi.advanceTimersByTimeAsync(10_000)
    await result
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock.mock.calls[0]?.[1]?.signal.aborted).toBe(true)
  })

  it('reads the current identity with same-origin credentials', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ logged_in: true }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await expect(getIdentity()).resolves.toEqual({ logged_in: true })
    expect(fetchMock).toHaveBeenCalledWith('/api/me', expect.objectContaining({ credentials: 'same-origin', signal: expect.any(AbortSignal) }))
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

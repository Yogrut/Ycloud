import { afterEach, describe, expect, it, vi } from 'vitest'
import { AdminApiError, getAdminInfo, updateAccount } from './admin'

afterEach(() => vi.unstubAllGlobals())

describe('admin API', () => {
  it('preserves an unauthorized status for the login gate', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      error: { message: 'Unauthorized' },
    }), { status: 401, headers: { 'Content-Type': 'application/json' } })))

    const error = await getAdminInfo().catch(reason => reason)
    expect(error).toBeInstanceOf(AdminApiError)
    expect(error.status).toBe(401)
  })

  it('sends only explicitly changed account fields', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await updateAccount({ password: 'new-administrator-password' })

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/account', expect.objectContaining({
      method: 'PUT',
      credentials: 'same-origin',
      body: JSON.stringify({ password: 'new-administrator-password' }),
    }))
  })
})

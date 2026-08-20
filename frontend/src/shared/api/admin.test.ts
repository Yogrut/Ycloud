import { afterEach, describe, expect, it, vi } from 'vitest'
import { AdminApiError, getAdminInfo, updateAccount, updateLoginRestriction, updateTransferLimits } from './admin'

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

  it('uses a scoped administrator endpoint for a manual IP restriction', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await updateLoginRestriction('block', 'admin', '192.0.2.10')

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/security/block', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ entry: 'admin', ip: '192.0.2.10' }),
    }))
  })

  it('sends all three transfer limits through the protected settings endpoint', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const body = { max_upload_bytes: 6_442_450_944, max_archive_bytes: 2_684_354_560, max_archive_entries: 750 }

    await updateTransferLimits(body)

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/limits', expect.objectContaining({
      method: 'PUT',
      credentials: 'same-origin',
      body: JSON.stringify(body),
    }))
  })
})

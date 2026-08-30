import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  AdminApiError,
  createFolderLock,
  createWebDavMount,
  deleteFolderLock,
  deleteWebDavMount,
  getAdminInfo,
  updateAccount,
  updateFolderLock,
  updateLoginRestriction,
  testS3Storage,
  updateTransferLimits,
  updateWebDavMount,
} from './admin'

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

  it('sends transfer size and bandwidth limits through the protected settings endpoint', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const body = {
      max_upload_bytes: 6_442_450_944,
      max_upload_batch_bytes: 21_474_836_480,
      max_upload_batch_entries: 1000,
      max_archive_bytes: 2_684_354_560,
      max_archive_entries: 750,
      upload_rate_bytes_per_sec: 8_388_608,
      download_rate_bytes_per_sec: 16_777_216,
    }

    await updateTransferLimits(body)

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/limits', expect.objectContaining({
      method: 'PUT',
      credentials: 'same-origin',
      body: JSON.stringify(body),
    }))
  })

  it('tests an S3 profile through the non-persisting administrator endpoint', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)
    const body = {
      provider: 'minio' as const,
      endpoint: 'https://rustfs.internal.example',
      bucket: 'ycloud',
      region: 'us-east-1',
      prefix: 'data/',
      addressing_style: 'path' as const,
      access_key_id: 'access',
      secret_access_key: 'secret',
      capacity_limit_bytes: null,
    }

    await testS3Storage(body)

    expect(fetchMock).toHaveBeenCalledWith('/api/admin/storage/test', expect.objectContaining({
      method: 'POST',
      credentials: 'same-origin',
      body: JSON.stringify(body),
    }))
  })

  it('uses the scoped folder-lock endpoints and accepts an empty delete response', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: 'lock-1', path: 'test' }), {
        status: 200, headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ id: 'lock-1', path: 'renamed' }), {
        status: 200, headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)

    await createFolderLock({ path: 'test', password: 'secure-lock-password' })
    await updateFolderLock('lock-1', { path: 'renamed' })
    await expect(deleteFolderLock('lock-1')).resolves.toBeUndefined()

    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/admin/locks', expect.objectContaining({
      method: 'POST', body: JSON.stringify({ path: 'test', password: 'secure-lock-password' }),
    }))
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/admin/locks/lock-1', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ path: 'renamed' }),
    }))
    expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/admin/locks/lock-1', expect.objectContaining({ method: 'DELETE' }))
  })

  it('uses the scoped WebDAV mount endpoints without exposing stored hashes', async () => {
    const view = {
      id: 'share-1', name: 'media', path: 'files', username: 'dav-user',
      webdav_enabled: true, has_password: true, readonly: false,
    }
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(view), {
        status: 200, headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...view, readonly: true }), {
        status: 200, headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', fetchMock)

    await createWebDavMount({
      name: 'media', path: 'files', username: 'dav-user', password: 'secure-dav-password',
      webdav_enabled: true, readonly: false,
    })
    await updateWebDavMount('share-1', { readonly: true })
    await expect(deleteWebDavMount('share-1')).resolves.toBeUndefined()

    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/admin/shares', expect.objectContaining({ method: 'POST' }))
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/admin/shares/share-1', expect.objectContaining({
      method: 'PUT', body: JSON.stringify({ readonly: true }),
    }))
    expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/admin/shares/share-1', expect.objectContaining({ method: 'DELETE' }))
  })
})

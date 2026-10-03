import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  AdminApiError,
  createFolderLock,
  createWebDavMount,
  deleteFolderLock,
  deleteWebDavMount,
  getAdminInfo,
  getDomainBinding,
  getTraffic,
  getLoginEvents,
  updateAccount,
  updateFolderLock,
  updateLoginRestriction,
  testS3Storage,
  updateTransferLimits,
  updateLocalStorage,
  updateS3Storage,
  updateWebDavMount,
  saveDomainBinding,
  removeDomainBinding,
  logoutSession,
} from './admin'

afterEach(() => vi.unstubAllGlobals())

describe('admin API', () => {
  it.each([
    [{ days: 7 }, 'days=7'],
    [{ start: '2026-09-26', end: '2026-10-02' }, 'start=2026-09-26&end=2026-10-02'],
    [{}, ''],
  ] as const)('serializes traffic range %j without deriving browser dates', async (range, query) => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('{}', { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    await getTraffic(range)
    expect(fetchMock).toHaveBeenCalledExactlyOnceWith('/api/admin/traffic?' + query, expect.objectContaining({ credentials: 'same-origin' }))
  })

  it('cancels an obsolete administrator info read through the shared client', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const controller = new AbortController()
    const result = getAdminInfo(controller.signal)
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
    const signal = fetchMock.mock.calls[0]![1].signal as AbortSignal
    controller.abort()
    await rejected
    expect(signal.aborted).toBe(true)
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock.mock.calls[0]![0]).toBe('/api/admin/info')
  })

  it('bounds the logout wait and does not automatically repeat it after a lost response', async () => {
    vi.useFakeTimers()
    try {
      const fetchMock = vi.fn().mockImplementation(() => new Promise<Response>(() => {}))
      vi.stubGlobal('fetch', fetchMock)
      const result = logoutSession()
      const rejected = expect(result).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
      expect(fetchMock).toHaveBeenCalledWith('/api/logout', expect.objectContaining({ method: 'POST', credentials: 'same-origin' }))
      await vi.advanceTimersByTimeAsync(330_000)
      await rejected
      expect(fetchMock.mock.calls[0]![1].signal.aborted).toBe(true)
      expect(fetchMock).toHaveBeenCalledTimes(1)
    } finally { vi.useRealTimers() }
  })

  it('cancels a disposed domain read through the shared request deadline', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const controller = new AbortController()
    const result = getDomainBinding(controller.signal)
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
    expect(fetchMock).toHaveBeenCalledWith('/api/admin/domain-binding', expect.objectContaining({ credentials: 'same-origin' }))
    const signal = fetchMock.mock.calls[0]![1].signal as AbortSignal
    expect(signal.aborted).toBe(false)
    controller.abort()
    await rejected
    expect(signal.aborted).toBe(true)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it.each(['save', 'remove'] as const)('does not retry an unconfirmed domain %s or infer it from administrator info', async operation => {
    const fetchMock = vi.fn().mockRejectedValue(new TypeError('connection lost'))
    vi.stubGlobal('fetch', fetchMock)
    const result = operation === 'save' ? saveDomainBinding({ public_url: 'https://cloud.example.com' }) : removeDomainBinding()
    await expect(result).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock).toHaveBeenCalledWith('/api/admin/domain-binding', expect.objectContaining({ method: operation === 'save' ? 'PUT' : 'DELETE' }))
  })

  it('cancels obsolete log reads through the existing request deadline without retrying', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise<Response>(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const controller = new AbortController()
    const result = getLoginEvents({ success: false, entry: 'admin', page: 2, limit: 50, search: '  Browser A  ' }, controller.signal)
    const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
    const [url, options] = fetchMock.mock.calls[0]!
    const params = new URL(url, 'http://localhost').searchParams
    expect(Object.fromEntries(params)).toEqual({ success: 'false', entry: 'admin', page: '2', limit: '50', search: 'Browser A' })
    expect(options.signal.aborted).toBe(false)
    controller.abort()
    await rejected
    expect(options.signal.aborted).toBe(true)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('reads back a lost settings response without repeating the write', async () => {
    const fetchMock = vi.fn()
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce(new Response(JSON.stringify({ max_upload_bytes: 123 }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(updateTransferLimits({ max_upload_bytes: 123 })).resolves.toEqual({ success: true })
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock.mock.calls[1]![0]).toBe('/api/admin/info')
    expect(fetchMock.mock.calls[1]![1].method).toBeUndefined()
  })

  it('keeps an unconfirmed settings result distinct from a rejected write', async () => {
    const fetchMock = vi.fn()
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce(new Response(JSON.stringify({ max_upload_bytes: 122 }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const error = await updateTransferLimits({ max_upload_bytes: 123 }).catch(reason => reason)
    expect(error.blocksRetry).toBe(true)
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })

  it('reads back a local storage edit from the actual administrator route', async () => {
    const fetchMock = vi.fn()
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce(new Response(JSON.stringify({ storage_instances: [{
        id: 'local-1', name: 'Archive', enabled: true, allow_guest_access: false,
        allow_guest_download: false,
        backend: { type: 'local', path: '/data/archive', capacity_limit_bytes: null },
      }] }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(updateLocalStorage('local-1', 'Archive', '/data/archive', null, true, false, false))
      .resolves.toEqual({ success: true })
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock.mock.calls[1]![0]).toBe('/api/admin/info')
  })

  it('does not infer an S3 credential edit from redacted administrator settings', async () => {
    const fetchMock = vi.fn().mockRejectedValueOnce(new TypeError('connection lost'))
    vi.stubGlobal('fetch', fetchMock)
    const result = updateS3Storage('s3-1', 'Objects', {
      provider: 'minio', endpoint: 'https://s3.example.test', bucket: 'files', region: 'us-east-1',
      prefix: 'data/', addressing_style: 'path', access_key_id: 'changed', secret_access_key: '',
      capacity_limit_bytes: null,
    }, true, false)
    await expect(result).rejects.toMatchObject({ blocksRetry: true })
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

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

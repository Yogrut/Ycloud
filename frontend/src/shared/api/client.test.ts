import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, errorMetadata, readJson, requestJson, requireSuccess, REQUEST_TIMEOUT_MS } from './client'
import { useLocale } from '../i18n'

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })

describe('API client primitives', () => {
  it('validates success at runtime rather than trusting the declared type', () => {
    expect(() => requireSuccess({ success: true })).not.toThrow()
    expect(() => requireSuccess({ success: false, message: 'write failed' })).toThrow('write failed')
    for (const receipt of [undefined, null, {}, '<html>proxy page</html>', { success: 1 }]) {
      try { requireSuccess(receipt); expect.unreachable() }
      catch (error) { expect(error).toMatchObject({ code: 'operation_result_unknown', blocksRetry: true }) }
    }
  })
  it('classifies an unqualified write timeout as unknown but preserves definite non-commit evidence', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'request_timeout' } }), { status: 408 })))
    await expect(requestJson('/write', { method: 'PUT' })).rejects.toMatchObject({ status: 408, code: 'operation_result_unknown', blocksRetry: true })
    const operation = { commit: 'not_committed', cleanup: 'complete', retry: 'after_correction' }
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: { code: 'request_timeout', operation } }), { status: 408 })))
    await expect(requestJson('/write', { method: 'PUT' })).resolves.toMatchObject({ body: { error: { operation } } })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(new Response('{}', { status: 408 })))
    await expect(requestJson('/read')).resolves.toMatchObject({ response: { status: 408 } })
  })
  it.each(['zh-CN', 'en'] as const)('localizes bare and generic permission denials in %s without losing metadata', locale => {
    const previous = useLocale().current.value
    useLocale().set(locale)
    try {
      const response = new Response(null, { status: 403, headers: { 'x-request-id': 'denied-request' } })
      const expected = locale === 'en' ? 'Permission denied for this operation' : '无权限执行此操作'
      expect(errorMetadata(response, undefined).message).toBe(expected)
      const operation = { commit: 'not_committed', cleanup: 'complete', retry: 'after_correction' } as const
      expect(errorMetadata(response, { error: { code: 'forbidden', message: 'Access denied', operation } })).toEqual({
        code: 'forbidden', message: expected, requestId: 'denied-request', operation,
      })
      expect(errorMetadata(response, { error: { code: 'forbidden', message: '没有创建目录的权限' } }).message).toBe('没有创建目录的权限')
      expect(errorMetadata(new Response(null, { status: 404 }), undefined).message).toBeUndefined()
    } finally { useLocale().set(previous) }
  })

  it('preserves a definite precondition failure without classifying it as an unknown commit', async () => {
    const response = new Response(JSON.stringify({ error: { code: 'precondition_failed', message: 'Refresh the file version', operation: { commit: 'not_committed', cleanup: 'complete', retry: 'after_correction' } } }), { status: 412, headers: { 'Content-Type': 'application/json' } })
    const metadata = errorMetadata(response, await readJson(response) ?? {})
    const error = new ApiError(metadata.message ?? 'failed', response.status, metadata.code, undefined, metadata.operation)
    expect(error.code).toBe('precondition_failed')
    expect(error.status).toBe(412)
    expect(error.blocksRetry).toBe(false)
    expect(error.operation?.retry).toBe('after_correction')
  })
  it('blocks repeat writes for committed and unknown results but preserves ordinary failures', () => {
    expect(new ApiError('pending', 409, 'operation_committed_pending').blocksRetry).toBe(true)
    expect(new ApiError('unknown', 0, 'operation_result_unknown').blocksRetry).toBe(true)
    expect(new ApiError('quota', 507, 'insufficient_storage').blocksRetry).toBe(false)
    const operation = { commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat' } as const
    const details = errorMetadata(new Response('', { status: 409 }), { error: { operation } })
    expect(new ApiError('pending', 409, undefined, undefined, details.operation).operation).toEqual(operation)
  })
  it('preserves the stable server error code and request id', async () => {
    const response = new Response(JSON.stringify({
      error: { code: 'insufficient_storage', message: 'No space' },
    }), {
      status: 507,
      headers: { 'Content-Type': 'application/json', 'x-request-id': 'request-123' },
    })
    const body = await readJson(response)
    const metadata = errorMetadata(response, body ?? {})
    const error = new ApiError(metadata.message ?? 'failed', response.status, metadata.code, metadata.requestId)

    expect(error.status).toBe(507)
    expect(error.code).toBe('insufficient_storage')
    expect(error.requestId).toBe('request-123')
  })
})

describe('network failure and request deadlines', () => {
  it('treats a truncated successful write response as unknown', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{"unfinished":')))
    await expect(requestJson('/api/mkdir', { method: 'POST' })).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
  })
  it('releases a stalled read and aborts its network request', async () => {
    vi.useFakeTimers()
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const request = requestJson('/api/files')
    const rejected = expect(request).rejects.toMatchObject({ code: 'request_timeout', blocksRetry: false })
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS)
    await rejected
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('treats a timed-out write as unknown, not safe to repeat', async () => {
    vi.useFakeTimers()
    vi.stubGlobal('fetch', vi.fn().mockImplementation(() => new Promise(() => {})))
    const rejected = expect(requestJson('/api/batch/delete', { method: 'POST' })).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS)
    await rejected
    expect(vi.getTimerCount()).toBe(0)
  })

  it('includes reading the response body in the deadline', async () => {
    vi.useFakeTimers()
    const response = new Response('{}')
    vi.spyOn(response, 'json').mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response))
    const rejected = expect(requestJson('/api/files')).rejects.toMatchObject({ code: 'request_timeout' })
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS)
    await rejected
  })

  it('preserves caller cancellation and removes the deadline timer', async () => {
    vi.useFakeTimers()
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const controller = new AbortController()
    const rejected = expect(requestJson('/api/files', { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' })
    controller.abort()
    await rejected
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not send an already-cancelled request', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const controller = new AbortController()
    controller.abort()
    await expect(requestJson('/api/files', { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it('does not retry or classify a lost write response as a confirmed failure', async () => {
    const fetchMock = vi.fn().mockRejectedValue(new TypeError('network disconnected'))
    vi.stubGlobal('fetch', fetchMock)
    await expect(requestJson('/api/mkdir', { method: 'POST' })).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('cleans up its deadline on a normal response', async () => {
    vi.useFakeTimers()
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{"success":true}')))
    expect((await requestJson('/api/files')).body).toEqual({ success: true })
    expect(vi.getTimerCount()).toBe(0)
  })
})

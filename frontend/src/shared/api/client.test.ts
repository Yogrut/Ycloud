import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, errorMetadata, readJson, requestJson, REQUEST_TIMEOUT_MS } from './client'

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })

describe('API client primitives', () => {
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

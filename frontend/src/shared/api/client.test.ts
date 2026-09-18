import { describe, expect, it } from 'vitest'
import { ApiError, errorMetadata, readJson } from './client'

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

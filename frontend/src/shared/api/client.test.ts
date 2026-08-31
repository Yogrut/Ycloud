import { describe, expect, it } from 'vitest'
import { ApiError, errorMetadata, readJson } from './client'

describe('API client primitives', () => {
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

import { afterEach, describe, expect, it, vi } from 'vitest'
import { useLocale } from '../i18n'
import { REQUEST_TIMEOUT_MS } from './client'
import { readTextPreview, TEXT_PREVIEW_BYTES } from './textPreview'

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  useLocale().set('zh-CN')
})

describe('bounded text preview reading', () => {
  it('requests the preview range with shared credentials and cancellation', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('hello'))
    vi.stubGlobal('fetch', fetchMock)
    const source = new AbortController()
    expect(await readTextPreview('/api/preview?path=notes.txt', source.signal)).toEqual({ text: 'hello', truncated: false })
    expect(fetchMock).toHaveBeenCalledWith('/api/preview?path=notes.txt', expect.objectContaining({
      credentials: 'same-origin', headers: { Range: 'bytes=0-2097151' }, signal: expect.any(AbortSignal),
    }))
  })

  it.each([
    ['bytes 0-7/8', false],
    ['bytes 0-7/80', true],
    ['bytes 0-7/*', false],
    ['invalid', false],
  ])('uses Content-Range %s only as evidence of a partial result', async (range, truncated) => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('complete', { status: 206, headers: { 'Content-Range': range } })))
    expect(await readTextPreview('/preview')).toEqual({ text: 'complete', truncated })
  })

  it('handles an empty body', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null)))
    expect(await readTextPreview('/preview')).toEqual({ text: '', truncated: false })
  })

  it('decodes multibyte characters split across body chunks', async () => {
    const encoded = new TextEncoder().encode('你好🙂')
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const byte of encoded) controller.enqueue(new Uint8Array([byte]))
        controller.close()
      },
    })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(stream)))
    expect(await readTextPreview('/preview')).toEqual({ text: '你好🙂', truncated: false })
  })

  it('caps an oversized single chunk even if the endpoint ignores Range', async () => {
    const cancel = vi.fn()
    const stream = new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new Uint8Array(TEXT_PREVIEW_BYTES + 64).fill(97)) },
      cancel,
    })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(stream)))
    const result = await readTextPreview('/preview')
    expect(result.text).toBe('a'.repeat(TEXT_PREVIEW_BYTES))
    expect(result.truncated).toBe(true)
    expect(cancel).toHaveBeenCalledOnce()
    expect(stream.locked).toBe(false)
  })

  it.each([false, true])('checks for bytes beyond an exactly full buffer (extra: %s)', async extra => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(TEXT_PREVIEW_BYTES).fill(97))
        if (extra) controller.enqueue(new Uint8Array([98]))
        controller.close()
      },
    })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(stream)))
    const result = await readTextPreview('/preview')
    expect(result.text.length).toBe(TEXT_PREVIEW_BYTES)
    expect(result.truncated).toBe(extra)
  })

  it('does not delay completion for unfinished body cancellation', async () => {
    const cancel = vi.fn().mockImplementation(() => new Promise(() => {}))
    const stream = new ReadableStream<Uint8Array>({
      start(controller) { controller.enqueue(new Uint8Array(TEXT_PREVIEW_BYTES + 1)) }, cancel,
    })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(stream)))
    expect((await readTextPreview('/preview')).truncated).toBe(true)
    expect(cancel).toHaveBeenCalledOnce()
    expect(stream.locked).toBe(false)
  })

  it.each([403, 404, 500])('preserves HTTP failure %s without displaying its body', async status => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('not a document', { status })))
    await expect(readTextPreview('/preview')).rejects.toMatchObject({ name: 'ApiError', status })
  })

  it.each(['zh-CN', 'en'] as const)('localizes an exhausted quota in %s', async language => {
    useLocale().set(language)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    await expect(readTextPreview('/preview')).rejects.toMatchObject({
      status: 429,
      message: language === 'zh-CN'
        ? '下载流量已用尽或剩余流量不足，请等待重置或联系管理员'
        : 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.',
    })
  })

  it('propagates a body read failure and releases the reader', async () => {
    const stream = new ReadableStream<Uint8Array>({ start(controller) { controller.error(new Error('body failed')) } })
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(stream)))
    await expect(readTextPreview('/preview')).rejects.toThrow('body failed')
    expect(stream.locked).toBe(false)
  })

  it('times out a stalled response body, not only the initial fetch', async () => {
    vi.useFakeTimers()
    let body!: ReadableStreamDefaultController<Uint8Array>
    const stream = new ReadableStream<Uint8Array>({ start(controller) { body = controller } })
    const fetchMock = vi.fn().mockResolvedValue(new Response(stream))
    vi.stubGlobal('fetch', fetchMock)
    const rejection = expect(readTextPreview('/preview')).rejects.toMatchObject({ code: 'request_timeout' })
    await vi.advanceTimersByTimeAsync(REQUEST_TIMEOUT_MS)
    await rejection
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
    // Native fetch cancels its body on abort; release this deliberately inert test stream.
    body.close()
  })

  it('does not fetch an already cancelled preview', async () => {
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const source = new AbortController()
    source.abort()
    await expect(readTextPreview('/preview', source.signal)).rejects.toMatchObject({ name: 'AbortError' })
    expect(fetchMock).not.toHaveBeenCalled()
  })

  it('preserves cancellation and removes the request timer', async () => {
    vi.useFakeTimers()
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const source = new AbortController()
    const rejection = expect(readTextPreview('/preview', source.signal)).rejects.toMatchObject({ name: 'AbortError' })
    source.abort()
    await rejection
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
  })
})

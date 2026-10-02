import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '../api/client'
import { readTextPreview, type TextPreview } from '../api/textPreview'
import { useLocale } from '../i18n'
import { useTextPreview } from './useTextPreview'

vi.mock('../api/textPreview', () => ({ readTextPreview: vi.fn() }))
const read = vi.mocked(readTextPreview)
const scopes: EffectScope[] = []

function deferred() {
  let resolve!: (value: TextPreview) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<TextPreview>((accept, decline) => { resolve = accept; reject = decline })
  return { promise, resolve, reject }
}

function setup(initial = '/preview/first') {
  const url = ref(initial)
  const scope = effectScope()
  scopes.push(scope)
  const preview = scope.run(() => useTextPreview(url))!
  return { preview, url, scope }
}

async function settle() {
  await Promise.resolve()
  await Promise.resolve()
}

beforeEach(() => { useLocale().set('zh-CN'); read.mockResolvedValue({ text: 'current', truncated: false }) })
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('text preview lifecycle', () => {
  it('does not read an empty target', () => {
    const { preview } = setup('')
    expect(read).not.toHaveBeenCalled()
    expect(preview.text.value).toBe('')
    expect(preview.ready.value).toBe(false)
  })

  it('loads the current target and preserves truncation', async () => {
    read.mockResolvedValue({ text: 'prefix', truncated: true })
    const { preview } = setup()
    expect(preview.ready.value).toBe(false)
    await settle()
    expect(preview.text.value).toBe('prefix')
    expect(preview.truncated.value).toBe(true)
    expect(preview.ready.value).toBe(true)
    expect(preview.error.value).toBe('')
  })

  it('does not repeat a read for an unchanged target', async () => {
    const { url } = setup()
    url.value = '/preview/first'
    await settle()
    expect(read).toHaveBeenCalledOnce()
  })

  it('clears old content immediately when switching targets', async () => {
    read.mockResolvedValueOnce({ text: 'old', truncated: true }).mockImplementationOnce(() => new Promise(() => {}))
    const { preview, url } = setup()
    await settle()
    const signal = read.mock.calls[0]?.[1]
    url.value = '/preview/second'
    expect(signal?.aborted).toBe(true)
    expect(preview.text.value).toBe('')
    expect(preview.ready.value).toBe(false)
    expect(preview.truncated.value).toBe(false)
  })

  it.each(['success', 'quota', 'failure'])('discards stale %s after changing the target', async outcome => {
    const old = deferred()
    read.mockReturnValueOnce(old.promise)
    const { preview, url } = setup()
    url.value = '/preview/second'
    await settle()
    if (outcome === 'success') old.resolve({ text: 'stale', truncated: true })
    else old.reject(outcome === 'quota' ? new ApiError('old quota', 429) : new Error('old error'))
    await settle()
    expect(preview.text.value).toBe('current')
    expect(preview.truncated.value).toBe(false)
    expect(preview.error.value).toBe('')
    expect(preview.trafficExhausted.value).toBe(false)
  })

  it('discards the first response even after returning to the original URL', async () => {
    const old = deferred()
    read.mockReturnValueOnce(old.promise)
    const { preview, url } = setup()
    url.value = '/preview/second'
    url.value = '/preview/first'
    await settle()
    old.resolve({ text: 'first old response', truncated: true })
    await settle()
    expect(preview.text.value).toBe('current')
    expect(preview.truncated.value).toBe(false)
    expect(read).toHaveBeenCalledTimes(3)
  })

  it('clears an exhausted quota when disabling and reopening the preview', async () => {
    read.mockRejectedValueOnce(new ApiError('quota', 429))
    const { preview, url } = setup()
    await settle()
    expect(preview.trafficExhausted.value).toBe(true)
    expect(preview.error.value).toBe('quota')
    url.value = ''
    expect(preview.error.value).toBe('')
    expect(preview.trafficExhausted.value).toBe(false)
    expect(read).toHaveBeenCalledOnce()
    url.value = '/preview/first'
    await settle()
    expect(preview.text.value).toBe('current')
  })

  it.each(['success', 'quota', 'failure'])('aborts on disposal and ignores late %s', async outcome => {
    const pending = deferred()
    read.mockReturnValueOnce(pending.promise)
    const { preview, scope, url } = setup()
    const signal = read.mock.calls[0]?.[1]
    scope.stop()
    expect(signal?.aborted).toBe(true)
    if (outcome === 'success') pending.resolve({ text: 'late', truncated: true })
    else pending.reject(outcome === 'quota' ? new ApiError('quota', 429) : new Error('late error'))
    url.value = '/preview/second'
    await settle()
    expect(read).toHaveBeenCalledOnce()
    expect(preview.text.value).toBe('')
    expect(preview.error.value).toBe('')
    expect(preview.ready.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
  })

  it.each(['zh-CN', 'en'] as const)('localizes a non-Error read rejection in %s', async language => {
    useLocale().set(language)
    read.mockRejectedValue(null)
    const { preview } = setup()
    await settle()
    expect(preview.error.value).toBe(useLocale().t('preview.failed'))
    expect(preview.ready.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
  })

  it('preserves a timeout as a read error without retrying automatically', async () => {
    read.mockRejectedValue(new ApiError('timed out', 0, 'request_timeout'))
    const { preview } = setup()
    await settle()
    expect(preview.error.value).toBe('timed out')
    expect(preview.ready.value).toBe(false)
    expect(read).toHaveBeenCalledOnce()
  })
})

import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { isPreviewTrafficExhausted } from '../api/browser'
import { useMediaPreview } from './useMediaPreview'

vi.mock('../api/browser', () => ({ isPreviewTrafficExhausted: vi.fn() }))
const check = vi.mocked(isPreviewTrafficExhausted)
const scopes: EffectScope[] = []

function deferred() {
  let resolve!: (value: boolean) => void
  const promise = new Promise<boolean>(accept => { resolve = accept })
  return { promise, resolve }
}

function setup(initial = '/preview?path=first&storage_id=primary', beforeLoad = false) {
  const url = ref(initial)
  const precheck = ref(beforeLoad)
  const scope = effectScope()
  scopes.push(scope)
  const preview = scope.run(() => useMediaPreview(url, precheck))!
  return { url, scope, preview, precheck }
}

async function settle() { await Promise.resolve(); await Promise.resolve() }

beforeEach(() => { check.mockResolvedValue(false) })
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.resetAllMocks()
})

describe('media preview lifecycle', () => {
  it('does not diagnose an empty target even on a stray error', async () => {
    const { preview } = setup('', true)
    await preview.onError()
    expect(check).not.toHaveBeenCalled()
    expect(preview.ready.value).toBe(false)
    expect(preview.failed.value).toBe(false)
  })

  it('does not probe media before a rendering error', () => {
    const { preview } = setup()
    expect(check).not.toHaveBeenCalled()
    expect(preview.ready.value).toBe(true)
    expect(preview.failed.value).toBe(false)
  })

  it.each([false, true])('keeps rendering failure separate from quota diagnosis (%s)', async exhausted => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { preview, url } = setup()
    const request = preview.onError()
    expect(preview.failed.value).toBe(true)
    expect(preview.ready.value).toBe(false)
    expect(check).toHaveBeenCalledExactlyOnceWith(url.value, expect.any(AbortSignal))
    pending.resolve(exhausted)
    await request
    expect(preview.failed.value).toBe(true)
    expect(preview.trafficExhausted.value).toBe(exhausted)
  })

  it('diagnoses repeated rendering errors only once per target', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { preview } = setup()
    const request = preview.onError()
    await preview.onError()
    pending.resolve(false)
    await request
    await preview.onError()
    expect(check).toHaveBeenCalledOnce()
  })

  it.each([false, true])('waits for PDF quota admission (%s) before showing the viewer', async exhausted => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { preview } = setup('/preview/report.pdf', true)
    expect(preview.ready.value).toBe(false)
    pending.resolve(exhausted)
    await settle()
    expect(preview.ready.value).toBe(!exhausted)
    expect(preview.failed.value).toBe(exhausted)
    expect(preview.trafficExhausted.value).toBe(exhausted)
  })

  it('does not probe an unchanged PDF target again', async () => {
    const { url, precheck } = setup('/preview/report.pdf', true)
    url.value = '/preview/report.pdf'
    precheck.value = true
    await settle()
    expect(check).toHaveBeenCalledOnce()
  })

  it('checks a PDF rendering failure once more without re-enabling its viewer', async () => {
    const { preview } = setup('/preview/report.pdf', true)
    await settle()
    expect(preview.ready.value).toBe(true)
    await preview.onError()
    await preview.onError()
    expect(preview.failed.value).toBe(true)
    expect(preview.ready.value).toBe(false)
    expect(check).toHaveBeenCalledTimes(2)
  })

  it('shares an in-flight PDF check with a rendering error instead of reviving the viewer', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { preview } = setup('/preview/report.pdf', true)
    await preview.onError()
    pending.resolve(false)
    await settle()
    expect(preview.failed.value).toBe(true)
    expect(preview.ready.value).toBe(false)
    expect(check).toHaveBeenCalledOnce()
  })

  it.each([false, true])('discards an old PDF admission response (%s) after changing storage', async exhausted => {
    const old = deferred()
    const next = deferred()
    check.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise)
    const { url, preview } = setup('/preview?path=report.pdf&storage_id=primary', true)
    const signal = check.mock.calls[0]?.[1]
    url.value = '/preview?path=report.pdf&storage_id=secondary'
    expect(signal?.aborted).toBe(true)
    old.resolve(exhausted)
    await settle()
    expect(preview.ready.value).toBe(false)
    expect(preview.failed.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
    next.resolve(false)
    await settle()
    expect(preview.ready.value).toBe(true)
  })

  it('clears a quota error on a new target', async () => {
    check.mockResolvedValueOnce(true)
    const { url, preview } = setup()
    await preview.onError()
    expect(preview.trafficExhausted.value).toBe(true)
    url.value = '/preview/second'
    expect(preview.failed.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
    expect(preview.ready.value).toBe(true)
    await preview.onError()
    expect(preview.trafficExhausted.value).toBe(false)
    expect(check).toHaveBeenCalledTimes(2)
  })

  it('invalidates the old check on media A → B → A', async () => {
    const old = deferred()
    check.mockReturnValueOnce(old.promise)
    const { url, preview } = setup()
    const firstUrl = url.value
    const request = preview.onError()
    url.value = '/preview/second'
    url.value = firstUrl
    old.resolve(true)
    await request
    expect(preview.failed.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
    await preview.onError()
    expect(check).toHaveBeenCalledTimes(2)
  })

  it('clears a PDF check when disabling preview', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { url, preview } = setup('/preview/report.pdf', true)
    const signal = check.mock.calls[0]?.[1]
    url.value = ''
    expect(signal?.aborted).toBe(true)
    pending.resolve(true)
    await settle()
    expect(preview.failed.value).toBe(false)
    expect(preview.ready.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
  })

  it('cancels a check when switching the loading policy for the same URL', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { precheck, preview } = setup('/preview/report.pdf', true)
    const signal = check.mock.calls[0]?.[1]
    precheck.value = false
    expect(signal?.aborted).toBe(true)
    pending.resolve(true)
    await settle()
    expect(preview.ready.value).toBe(true)
    expect(preview.failed.value).toBe(false)
  })

  it.each([false, true])('cancels PDF checks on scope disposal, ignores late %s and forbids new work', async exhausted => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { scope, url, preview } = setup('/preview/report.pdf', true)
    const signal = check.mock.calls[0]?.[1]
    scope.stop()
    expect(signal?.aborted).toBe(true)
    pending.resolve(exhausted)
    await settle()
    url.value = '/preview/other.pdf'
    await preview.onError()
    expect(check).toHaveBeenCalledOnce()
    expect(preview.ready.value).toBe(false)
    expect(preview.failed.value).toBe(false)
    expect(preview.trafficExhausted.value).toBe(false)
  })
})

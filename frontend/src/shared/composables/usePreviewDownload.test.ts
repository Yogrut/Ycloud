import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { checkDownload } from '../api/browser'
import { useLocale } from '../i18n'
import { usePreviewDownload } from './usePreviewDownload'

vi.mock('../api/browser', () => ({ checkDownload: vi.fn() }))
const check = vi.mocked(checkDownload)
const scopes: EffectScope[] = []

function deferred() {
  let resolve!: () => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<void>((accept, decline) => { resolve = accept; reject = decline })
  return { promise, resolve, reject }
}

function setup(initial = '/download?path=first&storage_id=primary') {
  const url = ref(initial)
  const scope = effectScope()
  scopes.push(scope)
  const download = scope.run(() => usePreviewDownload(url))!
  const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
  return { url, scope, download, navigate }
}

beforeEach(() => { useLocale().set('zh-CN'); check.mockResolvedValue(undefined) })
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.restoreAllMocks()
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('preview download lifecycle', () => {
  it('does not check on creation or with an empty target', async () => {
    const { download, navigate } = setup('')
    await download.startDownload()
    expect(check).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
  })

  it('checks before navigating to the exact target', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { url, download, navigate } = setup()
    const request = download.startDownload()
    expect(check).toHaveBeenCalledExactlyOnceWith(url.value, expect.any(AbortSignal))
    expect(navigate).not.toHaveBeenCalled()
    pending.resolve()
    await request
    expect(navigate).toHaveBeenCalledExactlyOnceWith(url.value)
  })

  it('ignores duplicate clicks while checking without queuing another download', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { download, navigate } = setup()
    const request = download.startDownload()
    await download.startDownload()
    await download.startDownload()
    pending.resolve()
    await request
    expect(check).toHaveBeenCalledOnce()
    expect(navigate).toHaveBeenCalledOnce()
  })

  it('allows another explicit download after the previous check finishes', async () => {
    const { download, navigate } = setup()
    await download.startDownload()
    await download.startDownload()
    expect(check).toHaveBeenCalledTimes(2)
    expect(navigate).toHaveBeenCalledTimes(2)
  })

  it.each(['success', 'failure'])('cancels the old %s when changing target and leaves the new check alone', async outcome => {
    const old = deferred()
    const next = deferred()
    check.mockReturnValueOnce(old.promise).mockReturnValueOnce(next.promise)
    const { download, url, navigate } = setup()
    const first = download.startDownload()
    const signal = check.mock.calls[0]?.[1]
    url.value = '/download?path=first&storage_id=other'
    expect(signal?.aborted).toBe(true)
    const second = download.startDownload()
    if (outcome === 'success') old.resolve()
    else old.reject(new Error('old failure'))
    await first
    await download.startDownload()
    expect(check).toHaveBeenCalledTimes(2)
    expect(download.error.value).toBe('')
    expect(navigate).not.toHaveBeenCalled()
    next.resolve()
    await second
    expect(navigate).toHaveBeenCalledExactlyOnceWith(url.value)
  })

  it('discards a check even after returning to its original URL', async () => {
    const old = deferred()
    check.mockReturnValueOnce(old.promise)
    const { download, url, navigate } = setup()
    const firstUrl = url.value
    const request = download.startDownload()
    url.value = '/download/other'
    url.value = firstUrl
    old.resolve()
    await request
    expect(navigate).not.toHaveBeenCalled()
    await download.startDownload()
    expect(navigate).toHaveBeenCalledExactlyOnceWith(firstUrl)
  })

  it('clears the download error when replacing the target', async () => {
    check.mockRejectedValue(new Error('permission denied'))
    const { download, url } = setup()
    await download.startDownload()
    expect(download.error.value).toBe('permission denied')
    url.value = '/download/other'
    expect(download.error.value).toBe('')
  })

  it.each(['success', 'failure'])('disables checks on scope disposal and ignores late %s', async outcome => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { download, url, scope, navigate } = setup()
    const request = download.startDownload()
    const signal = check.mock.calls[0]?.[1]
    scope.stop()
    expect(signal?.aborted).toBe(true)
    if (outcome === 'success') pending.resolve()
    else pending.reject(new Error('late failure'))
    await request
    url.value = '/download/other'
    await download.startDownload()
    expect(check).toHaveBeenCalledOnce()
    expect(download.error.value).toBe('')
    expect(navigate).not.toHaveBeenCalled()
  })

  it('cancels a pending check when disabling downloads', async () => {
    const pending = deferred()
    check.mockReturnValue(pending.promise)
    const { download, url, navigate } = setup()
    const request = download.startDownload()
    const signal = check.mock.calls[0]?.[1]
    url.value = ''
    expect(signal?.aborted).toBe(true)
    pending.resolve()
    await request
    await download.startDownload()
    expect(navigate).not.toHaveBeenCalled()
    expect(check).toHaveBeenCalledOnce()
  })

  it.each(['zh-CN', 'en'] as const)('localizes unexpected rejection in %s and allows a manual retry', async language => {
    useLocale().set(language)
    check.mockRejectedValueOnce(null)
    const { download, navigate } = setup()
    await download.startDownload()
    expect(download.error.value).toBe(language === 'zh-CN' ? '下载失败' : 'Download failed')
    expect(navigate).not.toHaveBeenCalled()
    expect(check).toHaveBeenCalledOnce()
    await download.startDownload()
    expect(download.error.value).toBe('')
    expect(navigate).toHaveBeenCalledOnce()
  })
})

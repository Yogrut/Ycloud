import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { checkDownload, downloadUrl, prepareArchive, type BrowserCapabilities } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { useBrowserDownloads } from './useBrowserDownloads'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  checkDownload: vi.fn(), prepareArchive: vi.fn(),
}))

const check = vi.mocked(checkDownload)
const prepare = vi.mocked(prepareArchive)
const scopes: EffectScope[] = []
const archive = {
  ticket: 'ticket + /', file_count: 2, entry_count: 3, total_bytes: 1024,
  max_bytes: 2048, max_entries: 10,
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup() {
  const scope = effectScope()
  scopes.push(scope)
  const capabilities: BrowserCapabilities = {
    download: true, upload: true, create_directory: true, rename: true,
    move_items: true, copy: true, delete: true,
  }
  const context = {
    storageId: ref('primary'), capabilities: ref(capabilities),
    maxArchiveBytes: ref(1024), maxArchiveEntries: ref(10), announce: vi.fn(),
  }
  const actions = scope.run(() => useBrowserDownloads(context))!
  const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
  return { scope, context, actions, navigate }
}

beforeEach(() => {
  useLocale().set('zh-CN')
  check.mockResolvedValue(undefined)
  prepare.mockResolvedValue(archive)
})

afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.restoreAllMocks()
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('browser downloads', () => {
  it('uses a preview confirmation storage snapshot rather than the current storage', async () => {
    const { context, actions, navigate } = setup()
    context.storageId.value = 'secondary'
    await actions.startDownload('old-file.zip', 'primary')
    const url = downloadUrl('old-file.zip', 'primary')
    expect(check).toHaveBeenCalledExactlyOnceWith(url)
    expect(navigate).toHaveBeenCalledExactlyOnceWith(url)
  })

  it('does not request anything on creation or without download permission', async () => {
    const { context, actions, navigate } = setup()
    expect(check).not.toHaveBeenCalled()
    expect(prepare).not.toHaveBeenCalled()
    context.capabilities.value.download = false
    await actions.startDownload('one.txt')
    await actions.startArchive(['one.txt'])
    expect(check).not.toHaveBeenCalled()
    expect(prepare).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('checks the frozen download URL before navigating even if the storage changes', async () => {
    const pending = deferred<void>()
    check.mockReturnValue(pending.promise)
    const { context, actions, navigate } = setup()
    const url = downloadUrl('folder/a + b.txt', 'primary')
    const request = actions.startDownload('folder/a + b.txt')
    context.storageId.value = 'other'
    expect(check).toHaveBeenCalledWith(url)
    expect(navigate).not.toHaveBeenCalled()
    pending.resolve()
    await request
    expect(navigate).toHaveBeenCalledExactlyOnceWith(url)
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('deduplicates the same pending URL but permits different files and storages', async () => {
    const pending = deferred<void>()
    check.mockReturnValue(pending.promise)
    const { context, actions, navigate } = setup()
    const first = actions.startDownload('one.txt')
    await actions.startDownload('one.txt')
    const second = actions.startDownload('two.txt')
    context.storageId.value = 'other'
    const third = actions.startDownload('one.txt')
    expect(check.mock.calls.map(([url]) => url)).toEqual([
      downloadUrl('one.txt', 'primary'), downloadUrl('two.txt', 'primary'), downloadUrl('one.txt', 'other'),
    ])
    pending.resolve()
    await Promise.all([first, second, third])
    expect(navigate).toHaveBeenCalledTimes(3)
    await actions.startDownload('one.txt')
    expect(check).toHaveBeenCalledTimes(4)
  })

  it.each([
    [new Error('Permission denied'), 'Permission denied'],
    ['offline', '下载失败'],
  ])('reports download rejection without navigating or automatically retrying', async (error, message) => {
    check.mockRejectedValue(error)
    const { context, actions, navigate } = setup()
    await actions.startDownload('one.txt')
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(message)
    expect(navigate).not.toHaveBeenCalled()
    expect(check).toHaveBeenCalledOnce()
    check.mockResolvedValue(undefined)
    await actions.startDownload('one.txt')
    expect(check).toHaveBeenCalledTimes(2)
    expect(navigate).toHaveBeenCalledOnce()
  })

  it('does not prepare an empty archive', async () => {
    const { context, actions, navigate } = setup()
    await actions.startArchive([])
    expect(prepare).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('freezes archive paths and storage, deduplicating preparation until it completes', async () => {
    const pending = deferred<typeof archive>()
    prepare.mockReturnValue(pending.promise)
    const { context, actions, navigate } = setup()
    const paths = ['one.txt', 'two.txt']
    const request = actions.startArchive(paths)
    paths.push('three.txt')
    context.storageId.value = 'other'
    await actions.startArchive(['other.txt'])
    expect(prepare).toHaveBeenCalledExactlyOnceWith(['one.txt', 'two.txt'], 'primary')
    expect(navigate).not.toHaveBeenCalled()
    pending.resolve(archive)
    await request
    expect(context.announce).toHaveBeenCalledExactlyOnceWith('正在打包 2 个文件、3 个条目（1.0 KB）', 'success')
    expect(navigate).toHaveBeenCalledExactlyOnceWith('/api/archive?ticket=ticket%20%2B%20%2F')
    await actions.startArchive(['other.txt'])
    expect(prepare).toHaveBeenCalledTimes(2)
    expect(prepare).toHaveBeenLastCalledWith(['other.txt'], 'other')
  })

  it.each([
    ['payload too large', '所选文件总大小超过 1.0 KB，请拆分选择'],
    ['大小超出限制', '所选文件总大小超过 1.0 KB，请拆分选择'],
    ['entry limit exceeded', '打包最多包含 10 个条目，请拆分选择'],
    ['条目超过限制', '打包最多包含 10 个条目，请拆分选择'],
  ])('keeps archive limit feedback bound to the requested storage: %s', async (message, expected) => {
    const pending = deferred<typeof archive>()
    prepare.mockReturnValue(pending.promise)
    const { context, actions, navigate } = setup()
    const request = actions.startArchive(['one.txt'])
    context.storageId.value = 'other'
    context.maxArchiveBytes.value = 2048
    context.maxArchiveEntries.value = 20
    pending.reject(new Error(message))
    await request
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(expected)
    expect(navigate).not.toHaveBeenCalled()
    expect(prepare).toHaveBeenCalledOnce()
    prepare.mockResolvedValue(archive)
    await actions.startArchive(['other.txt'])
    expect(prepare).toHaveBeenCalledTimes(2)
  })

  it.each([
    [new Error('Storage unavailable'), 'Storage unavailable'],
    ['offline', '打包准备失败'],
  ])('preserves archive errors without navigation or automatic retry', async (error, message) => {
    prepare.mockRejectedValue(error)
    const { context, actions, navigate } = setup()
    await actions.startArchive(['one.txt'])
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(message)
    expect(navigate).not.toHaveBeenCalled()
    expect(prepare).toHaveBeenCalledOnce()
  })

  it('preserves English download and archive feedback', async () => {
    const { context, actions } = setup()
    useLocale().set('en')
    check.mockRejectedValue('offline')
    await actions.startDownload('one.txt')
    expect(context.announce).toHaveBeenLastCalledWith('Download failed')
    prepare.mockRejectedValueOnce(new Error('entry limit'))
    await actions.startArchive(['one.txt'])
    expect(context.announce).toHaveBeenLastCalledWith('An archive can contain at most 10 entries; split the selection')
    prepare.mockRejectedValueOnce(new Error('too large'))
    await actions.startArchive(['one.txt'])
    expect(context.announce).toHaveBeenLastCalledWith('The selection exceeds 1.0 KB; split it into smaller groups')
    await actions.startArchive(['one.txt'])
    expect(context.announce).toHaveBeenLastCalledWith('Preparing 2 file(s), 3 entries (1.0 KB)', 'success')
  })

  it('keeps download checking and archive preparation independent', async () => {
    const pending = deferred<void>()
    check.mockReturnValue(pending.promise)
    const { actions, navigate } = setup()
    const request = actions.startDownload('one.txt')
    await actions.startArchive(['one.txt'])
    expect(prepare).toHaveBeenCalledOnce()
    expect(navigate).toHaveBeenCalledOnce()
    pending.resolve()
    await request
    expect(navigate).toHaveBeenCalledTimes(2)
  })

  it.each(['success', 'error'] as const)('ignores pending download %s after disposal and refuses new requests', async outcome => {
    const pending = deferred<void>()
    check.mockReturnValue(pending.promise)
    const { scope, context, actions, navigate } = setup()
    const request = actions.startDownload('one.txt')
    scope.stop()
    if (outcome === 'success') pending.resolve()
    else pending.reject(new Error('Offline'))
    await request
    await actions.startDownload('two.txt')
    await actions.startArchive(['two.txt'])
    expect(check).toHaveBeenCalledOnce()
    expect(prepare).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it.each(['success', 'error'] as const)('ignores pending archive %s after disposal without starting another preparation', async outcome => {
    const pending = deferred<typeof archive>()
    prepare.mockReturnValue(pending.promise)
    const { scope, context, actions, navigate } = setup()
    const request = actions.startArchive(['one.txt'])
    scope.stop()
    if (outcome === 'success') pending.resolve(archive)
    else pending.reject(new Error('Offline'))
    await request
    await actions.startArchive(['two.txt'])
    await actions.startDownload('two.txt')
    expect(prepare).toHaveBeenCalledOnce()
    expect(check).not.toHaveBeenCalled()
    expect(navigate).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })
})

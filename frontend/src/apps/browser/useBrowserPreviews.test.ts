import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { isPreviewTrafficExhausted, previewUrl, type FileEntry } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { useBrowserPreviews } from './useBrowserPreviews'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  isPreviewTrafficExhausted: vi.fn(),
}))

const check = vi.mocked(isPreviewTrafficExhausted)
const scopes: EffectScope[] = []

function entry(name: string, isDir = false): FileEntry {
  return { name, path: `folder/${name}`, is_dir: isDir, size: 1, modified: '', mime: '', icon: '', locked: false }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(accept => { resolve = accept })
  return { promise, resolve }
}

function setup() {
  const scope = effectScope()
  scopes.push(scope)
  const context = {
    storageId: ref('primary'),
    visibleEntries: ref([entry('first.JPG'), entry('second.webp'), entry('one.mp3'), entry('two.flac'), entry('notes.txt'), entry('fake.png', true)]),
    announce: vi.fn(),
  }
  const previews = scope.run(() => useBrowserPreviews(context))!
  return { context, previews, scope }
}

beforeEach(() => {
  useLocale().set('zh-CN')
  check.mockResolvedValue(false)
})
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('browser previews', () => {
  it('starts without previews or network work and uses the shared image rules', async () => {
    const { previews } = setup()
    expect(previews.filePreview.value).toBeNull()
    expect(previews.pendingDownload.value).toBeNull()
    expect(previews.audioPlayback.value).toBeNull()
    expect(previews.audioSource.value).toBe('')
    expect(previews.audioIndex.value).toBe(-1)
    expect(previews.activeGalleryEntry.value).toBeNull()
    expect(previews.activeGalleryIndex.value).toBe(-1)
    expect(previews.galleryImages.value.map(item => item.name)).toEqual(['first.JPG', 'second.webp'])
    await previews.onAudioError()
    expect(check).not.toHaveBeenCalled()
  })

  it('opens only visible gallery images and keeps navigation within the gallery', () => {
    const { previews } = setup()
    expect(previews.openPreviewImage(entry('absent.jpg'))).toBe(false)
    expect(previews.openPreviewImage(entry('fake.png', true))).toBe(false)
    previews.moveGalleryEntry(1)
    expect(previews.activeGalleryEntry.value).toBeNull()
    expect(previews.openPreviewImage(entry('first.JPG'))).toBe(true)
    expect(previews.galleryStorageId.value).toBe('primary')
    expect(previews.activeGalleryIndex.value).toBe(0)
    previews.moveGalleryEntry(-1)
    expect(previews.activeGalleryIndex.value).toBe(0)
    previews.moveGalleryEntry(1)
    expect(previews.activeGalleryEntry.value?.name).toBe('second.webp')
    previews.moveGalleryEntry(1)
    expect(previews.activeGalleryIndex.value).toBe(1)
    previews.closeGalleryEntry()
    expect(previews.activeGalleryEntry.value).toBeNull()
    expect(previews.galleryStorageId.value).toBe('')
  })

  it('does not reopen a vanished gallery target through navigation', () => {
    const { previews, context } = setup()
    previews.openPreviewImage(entry('first.JPG'))
    context.visibleEntries.value = [entry('replacement.png')]
    expect(previews.activeGalleryEntry.value).toBeNull()
    previews.moveGalleryEntry(1)
    expect(previews.activeGalleryEntry.value).toBeNull()
  })

  it.each(['notes.txt', 'page.html', 'report.pdf', 'clip.mp4'])('snapshots the file and storage for %s', name => {
    const { previews, context } = setup()
    const source = entry(name)
    previews.openPreviewFile(source)
    source.name = 'changed.txt'
    source.path = 'changed.txt'
    context.visibleEntries.value = []
    expect(previews.filePreview.value).toEqual({ entry: entry(name), storageId: 'primary' })
    expect(previews.pendingDownload.value).toBeNull()
    previews.closeFilePreview()
    expect(previews.filePreview.value).toBeNull()
  })

  it('keeps the unsupported download bound to its entry and clears it explicitly', () => {
    const { previews } = setup()
    const source = entry('archive.zip')
    previews.openPreviewFile(source)
    source.path = 'changed.zip'
    expect(previews.pendingDownload.value).toEqual({ entry: entry('archive.zip'), storageId: 'primary' })
    expect(previews.filePreview.value).toBeNull()
    previews.closePendingDownload()
    expect(previews.pendingDownload.value).toBeNull()
  })

  it('takes an independent audio queue snapshot and preserves it across same-storage listing changes', () => {
    const { previews, context } = setup()
    previews.openPreviewFile(context.visibleEntries.value[2]!)
    expect(previews.audioPlayback.value?.queue.map(item => item.name)).toEqual(['one.mp3', 'two.flac'])
    expect(previews.audioSource.value).toBe(previewUrl('folder/one.mp3', 'primary'))
    context.visibleEntries.value[3]!.path = 'changed.flac'
    context.visibleEntries.value = []
    previews.moveAudio(-1)
    expect(previews.audioIndex.value).toBe(0)
    previews.moveAudio(1)
    expect(previews.audioIndex.value).toBe(1)
    expect(previews.audioSource.value).toBe(previewUrl('folder/two.flac', 'primary'))
    previews.moveAudio(1)
    expect(previews.audioIndex.value).toBe(1)
    previews.closeAudio()
    expect(previews.audioSource.value).toBe('')
  })

  it('uses a one-track queue when no audio tracks remain in the listing', () => {
    const { previews, context } = setup()
    context.visibleEntries.value = []
    previews.openPreviewFile(entry('alone.wav'))
    expect(previews.audioPlayback.value?.queue).toEqual([entry('alone.wav')])
    expect(previews.audioIndex.value).toBe(0)
  })

  it('allows background audio for documents but stops it for video', () => {
    const { previews } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    previews.openPreviewFile(entry('notes.txt'))
    expect(previews.audioPlayback.value).not.toBeNull()
    previews.openPreviewFile(entry('clip.mp4'))
    expect(previews.audioPlayback.value).toBeNull()
    expect(previews.filePreview.value?.entry.name).toBe('clip.mp4')
  })

  it('keeps gallery, document and unsupported confirmation mutually exclusive', () => {
    const { previews } = setup()
    previews.openPreviewFile(entry('archive.zip'))
    previews.openPreviewImage(entry('first.JPG'))
    expect(previews.pendingDownload.value).toBeNull()
    previews.openPreviewFile(entry('notes.txt'))
    expect(previews.activeGalleryEntry.value).toBeNull()
    previews.openPreviewFile(entry('archive.zip'))
    expect(previews.filePreview.value).toBeNull()
    expect(previews.pendingDownload.value).not.toBeNull()
  })

  it.each(['gallery', 'document', 'download', 'audio'] as const)('clears %s state synchronously when storage changes', kind => {
    const { previews, context } = setup()
    if (kind === 'gallery') previews.openPreviewImage(entry('first.JPG'))
    else previews.openPreviewFile(entry(kind === 'document' ? 'notes.txt' : kind === 'audio' ? 'one.mp3' : 'archive.zip'))
    context.storageId.value = 'secondary'
    expect(previews.filePreview.value).toBeNull()
    expect(previews.pendingDownload.value).toBeNull()
    expect(previews.audioPlayback.value).toBeNull()
    expect(previews.activeGalleryEntry.value).toBeNull()
    expect(previews.galleryStorageId.value).toBe('')
  })

  it('checks a failed audio source once and creates its correctly bound download fallback', async () => {
    const pending = deferred<boolean>()
    check.mockReturnValue(pending.promise)
    const { previews, context } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    const request = previews.onAudioError()
    await previews.onAudioError()
    expect(check).toHaveBeenCalledExactlyOnceWith(previewUrl('folder/one.mp3', 'primary'))
    expect(previews.audioPlayback.value).toBeNull()
    pending.resolve(false)
    await request
    expect(previews.pendingDownload.value).toEqual({ entry: entry('one.mp3'), storageId: 'primary' })
    expect(context.announce).not.toHaveBeenCalled()
  })

  it.each(['zh-CN', 'en'] as const)('reports exhausted audio allowance without suggesting a download (%s)', async language => {
    useLocale().set(language)
    check.mockResolvedValue(true)
    const { previews, context } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    await previews.onAudioError()
    expect(previews.pendingDownload.value).toBeNull()
    expect(context.announce).toHaveBeenCalledExactlyOnceWith(expect.stringContaining(language === 'en' ? 'Download allowance' : '下载流量已用尽'))
  })

  it.each(['close', 'reopen', 'document', 'gallery', 'storage-return', 'dispose'] as const)('discards an obsolete audio diagnostic after %s', async action => {
    const pending = deferred<boolean>()
    check.mockReturnValue(pending.promise)
    const { previews, context, scope } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    const request = previews.onAudioError()
    if (action === 'close') previews.closeAudio()
    else if (action === 'reopen') { previews.openPreviewFile(entry('one.mp3')); previews.closeAudio() }
    else if (action === 'document') previews.openPreviewFile(entry('notes.txt'))
    else if (action === 'gallery') previews.openPreviewImage(entry('first.JPG'))
    else if (action === 'storage-return') { context.storageId.value = 'secondary'; context.storageId.value = 'primary' }
    else scope.stop()
    pending.resolve(false)
    await request
    expect(previews.pendingDownload.value).toBeNull()
    expect(context.announce).not.toHaveBeenCalled()
    if (action === 'document') expect(previews.filePreview.value?.entry.name).toBe('notes.txt')
    if (action === 'gallery') expect(previews.activeGalleryEntry.value?.name).toBe('first.JPG')
  })

  it('does not let an earlier quota result override a newly started audio track', async () => {
    const pending = deferred<boolean>()
    check.mockReturnValue(pending.promise)
    const { previews, context } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    const request = previews.onAudioError()
    previews.openPreviewFile(entry('two.flac'))
    pending.resolve(true)
    await request
    expect(context.announce).not.toHaveBeenCalled()
    expect(previews.audioPlayback.value?.entry.name).toBe('two.flac')
  })

  it('cleans disposed state and prevents new previews or diagnostics', async () => {
    const { previews, scope } = setup()
    previews.openPreviewFile(entry('one.mp3'))
    previews.openPreviewFile(entry('notes.txt'))
    scope.stop()
    expect(previews.filePreview.value).toBeNull()
    expect(previews.audioPlayback.value).toBeNull()
    previews.openPreviewFile(entry('notes.txt'))
    expect(previews.openPreviewImage(entry('first.JPG'))).toBe(false)
    previews.moveAudio(1)
    previews.moveGalleryEntry(1)
    await previews.onAudioError()
    expect(previews.filePreview.value).toBeNull()
    expect(previews.activeGalleryEntry.value).toBeNull()
    expect(check).not.toHaveBeenCalled()
  })
})

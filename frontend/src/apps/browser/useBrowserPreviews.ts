import { computed, onScopeDispose, ref, watch, type Ref } from 'vue'
import { isPreviewTrafficExhausted, previewUrl, type FileEntry } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { previewKind } from '../../shared/previewFormats'

interface BrowserPreviewsContext {
  storageId: Ref<string>
  visibleEntries: Readonly<Ref<FileEntry[]>>
  announce: (message: string) => void
}

interface PreviewTarget {
  entry: FileEntry
  storageId: string
}

interface AudioPlayback extends PreviewTarget {
  queue: FileEntry[]
}

export function useBrowserPreviews(context: BrowserPreviewsContext) {
  const locale = useLocale()
  const filePreview = ref<PreviewTarget | null>(null)
  const pendingDownload = ref<PreviewTarget | null>(null)
  const audioPlayback = ref<AudioPlayback | null>(null)
  const galleryTarget = ref<{ path: string; storageId: string } | null>(null)
  let revision = 0
  let disposed = false

  const galleryImages = computed(() => context.visibleEntries.value.filter(entry => !entry.is_dir && previewKind(entry.name) === 'image'))
  const activeGalleryIndex = computed(() => galleryImages.value.findIndex(entry => entry.path === galleryTarget.value?.path))
  const activeGalleryEntry = computed(() => galleryImages.value[activeGalleryIndex.value] ?? null)
  const galleryStorageId = computed(() => galleryTarget.value?.storageId ?? '')
  const audioIndex = computed(() => audioPlayback.value?.queue.findIndex(entry => entry.path === audioPlayback.value?.entry.path) ?? -1)
  const audioSource = computed(() => audioPlayback.value ? previewUrl(audioPlayback.value.entry.path, audioPlayback.value.storageId) : '')

  function closeFilePreview(): void {
    revision++
    filePreview.value = null
  }

  function closePendingDownload(): void {
    revision++
    pendingDownload.value = null
  }

  function closeAudio(): void {
    revision++
    audioPlayback.value = null
  }

  function closeGalleryEntry(): void {
    revision++
    galleryTarget.value = null
  }

  function openPreviewImage(entry: FileEntry): boolean {
    if (disposed || !galleryImages.value.some(image => image.path === entry.path)) return false
    revision++
    filePreview.value = null
    pendingDownload.value = null
    galleryTarget.value = { path: entry.path, storageId: context.storageId.value }
    return true
  }

  function openPreviewFile(entry: FileEntry): void {
    if (disposed) return
    revision++
    filePreview.value = null
    pendingDownload.value = null
    galleryTarget.value = null
    const target = { entry: { ...entry }, storageId: context.storageId.value }
    const kind = previewKind(entry.name)
    if (kind === 'audio') {
      const queue = context.visibleEntries.value
        .filter(item => !item.is_dir && previewKind(item.name) === 'audio')
        .map(item => ({ ...item }))
      audioPlayback.value = { ...target, queue: queue.length ? queue : [target.entry] }
    } else if (kind === 'unsupported') pendingDownload.value = target
    else {
      if (kind === 'video') audioPlayback.value = null
      filePreview.value = target
    }
  }

  function moveAudio(offset: number): void {
    const playback = audioPlayback.value
    if (disposed || !playback) return
    const entry = playback.queue[audioIndex.value + offset]
    if (entry) {
      revision++
      audioPlayback.value = { ...playback, entry }
    }
  }

  function moveGalleryEntry(offset: number): void {
    if (disposed || !galleryTarget.value || activeGalleryIndex.value < 0) return
    const entry = galleryImages.value[activeGalleryIndex.value + offset]
    if (entry) {
      revision++
      galleryTarget.value = { ...galleryTarget.value, path: entry.path }
    }
  }

  async function onAudioError(): Promise<void> {
    const playback = audioPlayback.value
    if (disposed || !playback) return
    const requestedRevision = ++revision
    audioPlayback.value = null
    const exhausted = await isPreviewTrafficExhausted(previewUrl(playback.entry.path, playback.storageId))
    // Matching only the file or current player misses close/reopen and A -> B -> A.
    if (disposed || requestedRevision !== revision) return
    if (exhausted) {
      context.announce(locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.'))
    } else pendingDownload.value = { entry: playback.entry, storageId: playback.storageId }
  }

  function clearPreviews(): void {
    revision++
    filePreview.value = null
    pendingDownload.value = null
    audioPlayback.value = null
    galleryTarget.value = null
  }

  watch(context.storageId, clearPreviews, { flush: 'sync' })
  onScopeDispose(() => {
    disposed = true
    clearPreviews()
  })

  return {
    filePreview, pendingDownload, audioPlayback, audioIndex, audioSource,
    galleryImages, activeGalleryIndex, activeGalleryEntry, galleryStorageId,
    openPreviewImage, openPreviewFile, moveAudio, onAudioError, closeAudio,
    moveGalleryEntry, closeGalleryEntry, closeFilePreview, closePendingDownload,
  }
}

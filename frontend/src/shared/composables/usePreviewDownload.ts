import { readonly, ref, watch, type Ref } from 'vue'
import { checkDownload } from '../api/browser'
import { useLocale } from '../i18n'

interface DownloadTarget {
  url: string
  controller: AbortController
  pending: boolean
}

// Preview downloads belong to the open target, unlike a background file task.
export function usePreviewDownload(url: Readonly<Ref<string>>) {
  const error = ref('')
  let current: DownloadTarget | undefined

  watch(url, (source, _previous, onCleanup) => {
    error.value = ''
    current = undefined
    if (!source) return
    const target: DownloadTarget = { url: source, controller: new AbortController(), pending: false }
    current = target
    onCleanup(() => {
      target.controller.abort()
      if (current === target) current = undefined
    })
  }, { immediate: true, flush: 'sync' })

  async function startDownload(): Promise<void> {
    const target = current
    if (!target || target.pending || target.controller.signal.aborted) return
    target.pending = true
    error.value = ''
    try {
      await checkDownload(target.url, target.controller.signal)
      if (!target.controller.signal.aborted) window.location.href = target.url
    } catch (reason) {
      if (!target.controller.signal.aborted) {
        error.value = reason instanceof Error ? reason.message : useLocale().text('下载失败', 'Download failed')
      }
    } finally {
      target.pending = false
    }
  }

  return { error: readonly(error), startDownload }
}

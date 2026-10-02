import { onScopeDispose, type Ref } from 'vue'
import { checkDownload, downloadUrl, prepareArchive, type BrowserCapabilities } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'

interface BrowserDownloadsContext {
  storageId: Ref<string>
  capabilities: Ref<BrowserCapabilities>
  maxArchiveBytes: Ref<number>
  maxArchiveEntries: Ref<number>
  announce: (message: string, kind?: 'error' | 'success') => void
}

export function useBrowserDownloads(context: BrowserDownloadsContext) {
  const locale = useLocale()
  const pendingDownloads = new Set<string>()
  let archivePreparing = false
  let disposed = false

  async function startDownload(entryPath: string, storageId = context.storageId.value): Promise<void> {
    if (disposed || !context.capabilities.value.download) return
    const url = downloadUrl(entryPath, storageId)
    if (pendingDownloads.has(url)) return
    pendingDownloads.add(url)
    try {
      await checkDownload(url)
      if (!disposed) window.location.href = url
    } catch (error) {
      if (!disposed) context.announce(error instanceof Error ? error.message : locale.text('下载失败', 'Download failed'))
    } finally {
      pendingDownloads.delete(url)
    }
  }

  async function startArchive(paths: string[]): Promise<void> {
    if (disposed || archivePreparing || !paths.length || !context.capabilities.value.download) return
    // Keep both the request and its feedback bound to the selected storage.
    const storageId = context.storageId.value
    const maxBytes = context.maxArchiveBytes.value
    const maxEntries = context.maxArchiveEntries.value
    archivePreparing = true
    try {
      const result = await prepareArchive([...paths], storageId)
      if (disposed) return
      context.announce(locale.text(
        `正在打包 ${result.file_count} 个文件、${result.entry_count} 个条目（${formatSize(result.total_bytes)}）`,
        `Preparing ${result.file_count} file(s), ${result.entry_count} entries (${formatSize(result.total_bytes)})`,
      ), 'success')
      window.location.href = `/api/archive?ticket=${encodeURIComponent(result.ticket)}`
    } catch (error) {
      if (disposed) return
      const message = error instanceof Error ? error.message : locale.text('打包准备失败', 'Unable to prepare the archive')
      if (/payload too large|too large|大小/i.test(message)) {
        context.announce(locale.text(
          `所选文件总大小超过 ${formatSize(maxBytes)}，请拆分选择`,
          `The selection exceeds ${formatSize(maxBytes)}; split it into smaller groups`,
        ))
      } else if (/entry limit|条目/i.test(message)) {
        context.announce(locale.text(
          `打包最多包含 ${maxEntries} 个条目，请拆分选择`,
          `An archive can contain at most ${maxEntries} entries; split the selection`,
        ))
      } else context.announce(message)
    } finally {
      archivePreparing = false
    }
  }

  onScopeDispose(() => {
    // Requests retain the API client's deadline; leaving the page only stops
    // their later feedback/navigation, not an already accepted archive task.
    disposed = true
    pendingDownloads.clear()
  })

  return { startDownload, startArchive }
}

import { computed, ref } from 'vue'
import type { Ref } from 'vue'
import type { BatchOperation, BatchResponse, BrowserCapabilities, FileEntry } from '../../shared/api/browser'
import { batchOperation, checkDownload, createFolder, downloadUrl, prepareArchive, renameItem } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import { batchSummary } from './operationFeedback'

interface BrowserFileOperationsContext {
  path: Ref<string>
  storageId: Ref<string>
  entries: Ref<FileEntry[]>
  capabilities: Ref<BrowserCapabilities>
  maxArchiveBytes: Ref<number>
  maxArchiveEntries: Ref<number>
  selected: Ref<Set<string>>
  requireCapability: (action: keyof BrowserCapabilities) => boolean
  announce: (message: string, kind?: 'error' | 'success') => void
  refresh: () => Promise<void>
}

export function useBrowserFileOperations(context: BrowserFileOperationsContext) {
  const locale = useLocale()
  const showFolder = ref(false)
  const folderName = ref('')
  const folderError = ref('')
  const creatingFolder = ref(false)
  const showRename = ref(false)
  const renameTarget = ref('')
  const renameName = ref('')
  const renameError = ref('')
  const renaming = ref(false)
  const showDelete = ref(false)
  const pendingDelete = ref<string[]>([])
  const operationBusy = ref(false)
  const operationError = ref('')
  const pickerOperation = ref<'move' | 'copy' | null>(null)
  const pickerPaths = ref<string[]>([])
  const pickerStorageId = ref('')
  const batchResult = ref<BatchResponse | null>(null)

  const pickerTitle = computed(() => locale.text(
    `${pickerOperation.value === 'move' ? '移动' : '复制'} ${pickerPaths.value.length} 个项目到…`,
    `${pickerOperation.value === 'move' ? 'Move' : 'Copy'} ${pickerPaths.value.length} item(s) to…`,
  ))

  function entryForPath(entryPath: string): FileEntry | undefined {
    return context.entries.value.find(entry => entry.path === entryPath)
  }

  function openFolderDialog(): void {
    if (!context.requireCapability('create_directory')) return
    folderName.value = ''
    folderError.value = ''
    showFolder.value = true
  }

  async function submitFolder(): Promise<void> {
    const name = folderName.value.trim()
    if (!name || creatingFolder.value) return
    creatingFolder.value = true
    folderError.value = ''
    try {
      await createFolder(context.path.value, name, context.storageId.value)
      showFolder.value = false
      context.announce(locale.text('文件夹已创建', 'Folder created'), 'success')
      await context.refresh()
    } catch (error) {
      folderError.value = error instanceof Error ? error.message : locale.text('创建失败', 'Unable to create the folder')
    } finally {
      creatingFolder.value = false
    }
  }

  async function startDownload(entryPath: string): Promise<void> {
    if (!context.capabilities.value.download) return
    try {
      const url = downloadUrl(entryPath, context.storageId.value)
      await checkDownload(url)
      window.location.href = url
    } catch (error) {
      context.announce(error instanceof Error ? error.message : locale.text('下载失败', 'Download failed'))
    }
  }

  async function startArchive(paths: string[]): Promise<void> {
    if (!paths.length || !context.capabilities.value.download) return
    try {
      const result = await prepareArchive(paths, context.storageId.value)
      context.announce(locale.text(
        `正在打包 ${result.file_count} 个文件、${result.entry_count} 个条目（${formatSize(result.total_bytes)}）`,
        `Preparing ${result.file_count} file(s), ${result.entry_count} entries (${formatSize(result.total_bytes)})`,
      ), 'success')
      window.location.href = `/api/archive?ticket=${encodeURIComponent(result.ticket)}`
    } catch (error) {
      const message = error instanceof Error ? error.message : locale.text('打包准备失败', 'Unable to prepare the archive')
      if (/payload too large|too large|大小/i.test(message)) {
        context.announce(locale.text(
          `所选文件总大小超过 ${formatSize(context.maxArchiveBytes.value)}，请拆分选择`,
          `The selection exceeds ${formatSize(context.maxArchiveBytes.value)}; split it into smaller groups`,
        ))
      } else if (/entry limit|条目/i.test(message)) {
        context.announce(locale.text(
          `打包最多包含 ${context.maxArchiveEntries.value} 个条目，请拆分选择`,
          `An archive can contain at most ${context.maxArchiveEntries.value} entries; split the selection`,
        ))
      } else context.announce(message)
    }
  }

  function openRenameDialog(entryPath: string): void {
    if (!context.requireCapability('rename')) return
    renameTarget.value = entryPath
    renameName.value = entryForPath(entryPath)?.name ?? entryPath.split('/').pop() ?? ''
    renameError.value = ''
    showRename.value = true
  }

  async function submitRename(): Promise<void> {
    const name = renameName.value.trim()
    if (!name || renaming.value) return
    renaming.value = true
    renameError.value = ''
    try {
      await renameItem(context.path.value, renameTarget.value, name, context.storageId.value)
      showRename.value = false
      context.announce(locale.text('重命名成功', 'Renamed'), 'success')
      await context.refresh()
    } catch (error) {
      renameError.value = error instanceof Error ? error.message : locale.text('重命名失败', 'Unable to rename the item')
    } finally {
      renaming.value = false
    }
  }

  function requestTransfer(operation: 'move' | 'copy', paths: string[]): void {
    if (!paths.length || !context.requireCapability(operation === 'move' ? 'move_items' : 'copy')) return
    pickerOperation.value = operation
    pickerPaths.value = [...paths]
    pickerStorageId.value = context.storageId.value
    operationError.value = ''
  }

  function requestDelete(paths: string[]): void {
    if (!paths.length || !context.requireCapability('delete')) return
    pendingDelete.value = [...paths]
    operationError.value = ''
    showDelete.value = true
  }

  async function runBatch(operation: BatchOperation, paths: string[], target = '', storageId = context.storageId.value): Promise<void> {
    operationBusy.value = true
    operationError.value = ''
    try {
      const result = await batchOperation(operation, paths, target, storageId)
      const label = operation === 'move'
        ? locale.text('移动', 'Move')
        : operation === 'copy' ? locale.text('复制', 'Copy') : locale.text('删除', 'Delete')
      if (result.failed) batchResult.value = result
      context.announce(result.failed
        ? locale.text(`${label}${batchSummary(result)}`, `${label}: ${batchSummary(result, true)}`)
        : locale.text(`${label}成功`, `${label} completed`), result.failed ? 'error' : 'success')
      context.selected.value = new Set()
      await context.refresh()
    } catch (error) {
      operationError.value = error instanceof Error ? error.message : locale.t('common.failed')
      context.announce(operationError.value)
    } finally {
      operationBusy.value = false
    }
  }

  async function confirmTransfer(target: string): Promise<void> {
    const operation = pickerOperation.value
    const paths = [...pickerPaths.value]
    const storageId = pickerStorageId.value
    pickerOperation.value = null
    if (operation) await runBatch(operation, paths, target, storageId)
  }

  async function confirmDelete(): Promise<void> {
    const paths = [...pendingDelete.value]
    await runBatch('delete', paths)
    if (!operationError.value) showDelete.value = false
  }

  return {
    batchResult,
    confirmDelete,
    confirmTransfer,
    creatingFolder,
    entryForPath,
    folderError,
    folderName,
    openFolderDialog,
    openRenameDialog,
    operationBusy,
    pendingDelete,
    pickerOperation,
    pickerStorageId,
    pickerTitle,
    renameError,
    renameName,
    renaming,
    requestDelete,
    requestTransfer,
    showDelete,
    showFolder,
    showRename,
    startArchive,
    startDownload,
    submitFolder,
    submitRename,
  }
}

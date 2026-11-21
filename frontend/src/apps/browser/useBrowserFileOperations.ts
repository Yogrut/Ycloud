import { computed, onScopeDispose, ref } from 'vue'
import type { Ref } from 'vue'
import type { BatchOperation, BatchResponse, BrowserCapabilities, ConflictPolicy } from '../../shared/api/browser'
import { batchOperation } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { ApiError } from '../../shared/api/client'
import { buildBatchReport, formatBatchSummary, type BatchReport } from './operationFeedback'

interface BrowserFileOperationsContext {
  storageId: Ref<string>
  selected: Ref<Set<string>>
  requireCapability: (action: keyof BrowserCapabilities) => boolean
  announce: (message: string, kind?: 'error' | 'success') => void
  refresh: (deletion?: { storageId: string; paths: string[] }) => Promise<void>
  /// Silent refresh that ignores the minimum interval and queues behind a
  /// visible one instead of cancelling it.
  refreshForced: () => Promise<void>
  hideEntries: (storageId: string, paths: string[]) => void
  releaseEntries: (storageId: string, paths: string[]) => void
  /// Marks the operation as settled: the rows stay hidden until a listing that
  /// started afterwards is applied.
  settleEntries: (storageId: string, paths: string[]) => void
}

interface BatchRequest {
  operation: BatchOperation
  paths: string[]
  storageId: string
}

export function useBrowserFileOperations(context: BrowserFileOperationsContext) {
  const locale = useLocale()
  const pendingRequest = ref<BatchRequest | null>(null)
  const operationBusy = ref(false)
  const operationError = ref('')
  const operationRetryBlocked = ref(false)
  const batchResult = ref<BatchReport | null>(null)
  /// Batches still running in the background; drives the toolbar counter.
  const pendingBatches = ref(0)
  let disposed = false

  function displayName(path: string): string {
    return path.replace(/^\/+|\/+$/g, '').split('/').pop() ?? path
  }

  /// Names the item a failed request was about. A single item is already obvious
  /// from the dialog it came from, so only batches need the extra context.
  function describePaths(paths: string[]): string {
    if (paths.length < 2) return ''
    const first = displayName(paths[0] ?? '')
    if (!first) return ''
    return locale.text(`（${first} 等 ${paths.length} 项）`, ` (${first} and ${paths.length - 1} more)`)
  }

  const showDelete = computed(() => pendingRequest.value?.operation === 'delete')
  const pendingDelete = computed(() => {
    const request = pendingRequest.value
    return request?.operation === 'delete' ? request.paths : []
  })
  const pickerOperation = computed(() => {
    const operation = pendingRequest.value?.operation
    return operation === 'move' || operation === 'copy' ? operation : null
  })
  const pickerStorageId = computed(() => pickerOperation.value ? pendingRequest.value?.storageId ?? '' : '')

  const pickerTitle = computed(() => locale.text(
    pickerOperation.value === 'move' ? '移动到…' : '复制到…',
    pickerOperation.value === 'move' ? 'Move to…' : 'Copy to…',
  ))

  function openBatchDialog(operation: BatchOperation, paths: string[]): void {
    const capability = operation === 'move' ? 'move_items' : operation
    if (disposed || operationBusy.value || !paths.length || !context.requireCapability(capability)) return
    pendingRequest.value = { operation, paths: [...paths], storageId: context.storageId.value }
    operationError.value = ''
    operationRetryBlocked.value = false
  }

  function requestTransfer(operation: 'move' | 'copy', paths: string[]): void {
    openBatchDialog(operation, paths)
  }

  function requestDelete(paths: string[]): void {
    openBatchDialog('delete', paths)
  }

  function closeBatchDialog(): void {
    if (operationBusy.value) return
    pendingRequest.value = null
    operationError.value = ''
    operationRetryBlocked.value = false
  }

  async function runBatch(request: BatchRequest, target = '', conflict: ConflictPolicy = 'error'): Promise<void> {
    const { operation, paths, storageId } = request
    // Delete and move hide their rows at once; a failure releases them again and
    // a silent refresh brings them back, so the list never blanks or flickers.
    const hidesRows = operation === 'delete' || operation === 'move'
    if (hidesRows) context.hideEntries(storageId, paths)
    pendingBatches.value += 1
    operationBusy.value = true
    operationError.value = ''
    let result: BatchResponse
    try {
      result = await batchOperation(operation, [...paths], target, storageId, conflict)
    } catch (error) {
      if (hidesRows) {
        context.releaseEntries(storageId, paths)
        if (!disposed) void context.refreshForced().catch(() => undefined)
      }
      if (!disposed) {
        operationRetryBlocked.value = error instanceof ApiError && error.blocksRetry
        operationError.value = (error instanceof Error ? error.message : locale.t('common.failed')) + describePaths(paths)
        context.announce(operationError.value)
      }
      return
    } finally {
      pendingBatches.value = Math.max(0, pendingBatches.value - 1)
      operationBusy.value = false
    }
    if (disposed) return
    closeBatchDialog()
    const report = buildBatchReport(result)
    const label = operation === 'move'
      ? locale.text('移动', 'Move')
      : operation === 'copy' ? locale.text('复制', 'Copy') : locale.text('删除', 'Delete')
    const needsAttention = report.attentionItems.length > 0
    batchResult.value = needsAttention ? report : null
    context.announce(needsAttention
      ? locale.text(`${label}${formatBatchSummary(report)}`, `${label}: ${formatBatchSummary(report, true)}`)
      : locale.text(`${label}成功`, `${label} completed`), report.failed || report.unknown ? 'error' : 'success')
    context.selected.value = new Set()
    // Rows that did not succeed must stop being hidden before the refresh, so
    // the server copy of them can come back.
    const retainedPaths = result.results
      .filter(item => item.status < 400 || item.operation?.commit === 'committed')
      .map(item => item.path)
    if (hidesRows) {
      const retained = new Set(retainedPaths)
      context.releaseEntries(storageId, paths.filter(path => !retained.has(path)))
    }
    // Refresh has its own outcome; it cannot make a completed batch retryable.
    try {
      if (operation === 'delete') {
        const paths = result.results
          .filter(item => item.status >= 200 && item.status < 300 || item.operation?.commit === 'committed')
          .map(item => item.path)
        await context.refresh({ storageId, paths })
      } else await context.refreshForced()
    } catch (error) {
      if (!disposed) context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
    } finally {
      // Successful rows stay hidden until a listing that started after this point
      // is applied; an older response must never put them back.
      if (hidesRows) context.settleEntries(storageId, retainedPaths)
    }
  }

  async function confirmTransfer(target: string, conflict: ConflictPolicy = 'error'): Promise<void> {
    const request = pendingRequest.value
    if (disposed || operationBusy.value || !request || request.operation === 'delete') return
    // Preserve the existing picker-close behavior before starting a transfer.
    pendingRequest.value = null
    await runBatch(request, target, conflict)
  }

  async function confirmDelete(): Promise<void> {
    const request = pendingRequest.value
    if (disposed || operationBusy.value || operationRetryBlocked.value || request?.operation !== 'delete') return
    await runBatch(request)
  }

  onScopeDispose(() => {
    disposed = true
    pendingRequest.value = null
    batchResult.value = null
    operationError.value = ''
    operationRetryBlocked.value = false
    // Accepted writes are not aborted; ignore their later page-side results.
  })

  return {
    batchResult,
    closeBatchDialog,
    confirmDelete,
    confirmTransfer,
    operationBusy,
    operationError,
    operationRetryBlocked,
    pendingBatches,
    pendingDelete,
    pickerOperation,
    pickerStorageId,
    pickerTitle,
    requestDelete,
    requestTransfer,
    showDelete,
  }
}

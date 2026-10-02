import { computed, onScopeDispose, ref } from 'vue'
import type { Ref } from 'vue'
import type { BatchOperation, BatchResponse, BrowserCapabilities } from '../../shared/api/browser'
import { batchOperation } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { ApiError } from '../../shared/api/client'
import { buildBatchReport, formatBatchSummary, type BatchReport } from './operationFeedback'

interface BrowserFileOperationsContext {
  storageId: Ref<string>
  selected: Ref<Set<string>>
  requireCapability: (action: keyof BrowserCapabilities) => boolean
  announce: (message: string, kind?: 'error' | 'success') => void
  refresh: () => Promise<void>
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
  let disposed = false

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
    `${pickerOperation.value === 'move' ? '移动' : '复制'} ${pendingRequest.value?.paths.length ?? 0} 个项目到…`,
    `${pickerOperation.value === 'move' ? 'Move' : 'Copy'} ${pendingRequest.value?.paths.length ?? 0} item(s) to…`,
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

  async function runBatch(request: BatchRequest, target = ''): Promise<void> {
    const { operation, paths, storageId } = request
    operationBusy.value = true
    operationError.value = ''
    let result: BatchResponse
    try {
      result = await batchOperation(operation, [...paths], target, storageId)
    } catch (error) {
      if (!disposed) {
        operationRetryBlocked.value = error instanceof ApiError && error.blocksRetry
        operationError.value = error instanceof Error ? error.message : locale.t('common.failed')
        context.announce(operationError.value)
      }
      return
    } finally {
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
    // Refresh has its own outcome; it cannot make a completed batch retryable.
    try {
      await context.refresh()
    } catch (error) {
      if (!disposed) context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
    }
  }

  async function confirmTransfer(target: string): Promise<void> {
    const request = pendingRequest.value
    if (disposed || operationBusy.value || !request || request.operation === 'delete') return
    // Preserve the existing picker-close behavior before starting a transfer.
    pendingRequest.value = null
    await runBatch(request, target)
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
    pendingDelete,
    pickerOperation,
    pickerStorageId,
    pickerTitle,
    requestDelete,
    requestTransfer,
    showDelete,
  }
}

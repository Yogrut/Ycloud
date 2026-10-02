import { computed, ref } from 'vue'
import type { Ref } from 'vue'
import { cancelUploadBatch, getUploadBatchStatus, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import type { UploadBatchItemState, UploadBatchStatus } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import { candidatesFromDrop, candidatesFromFiles, joinUploadPath, MAX_UPLOAD_QUEUE_TASKS, UploadSelectionLimitError } from './uploadCandidates'
import type { UploadCandidate } from './uploadCandidates'
import type { UploadTask } from './uploadQueue'
import { canUploadTaskAction, isUploadPathReserved } from './uploadQueue'
import { groupUploadTasksByStorage, groupUploadTasksByTicket, uploadTicketKey } from './uploadTaskGroups'
import { applyUploadServerState, markUploadUnconfirmed } from './uploadTaskState'
import { createUploadReconciliation, indexUploadBatchItems } from './uploadReconciliation'
import { ApiError } from '../../shared/api/client'

interface UploadQueueContext {
  storageId: Ref<string>
  path: Ref<string>
  maxUploadBytes: Ref<number>
  maxUploadBatchBytes: Ref<number>
  maxUploadBatchEntries: Ref<number>
  canUpload: () => boolean
  requireUpload: () => boolean
  announce: (message: string) => void
  refresh: () => Promise<void>
}

interface UploadDestination {
  storageId: string
  basePath: string
}

export function useUploadQueue(context: UploadQueueContext) {
  const locale = useLocale()
  const fileInput = ref<HTMLInputElement>()
  const folderInput = ref<HTMLInputElement>()
  const filePanel = ref<HTMLElement>()
  const showUpload = ref(false)
  const uploading = ref(false)
  const uploadTasks = ref<UploadTask[]>([])
  const retainedUploadTasks = computed(() => new Set(uploadTasks.value))
  const uploadDropActive = ref(false)
  let uploadTaskSequence = 0
  let uploadDragDepth = 0
  let currentUploadController: AbortController | undefined
  let currentUploadTaskId: number | undefined
  let disposed = false
  const selectionControllers = new Set<AbortController>()
  let refreshing = false
  let pendingRefresh: UploadDestination | undefined
  const reconciliation = createUploadReconciliation({
    tasks: () => retainedUploadTasks.value,
    applyResult: (task, item) => applyServerUploadState(task, item.status, item.operation),
    unconfirmed: markUnconfirmed,
  })

  function chooseFiles(): void {
    if (context.requireUpload()) fileInput.value?.click()
  }

  function setFileInput(element: unknown): void {
    fileInput.value = element instanceof HTMLInputElement ? element : undefined
  }

  function setFolderInput(element: unknown): void {
    folderInput.value = element instanceof HTMLInputElement ? element : undefined
  }

  function chooseFolder(): void {
    if (context.requireUpload()) folderInput.value?.click()
  }

  function openUploadManager(): void {
    if (!context.requireUpload()) return
    showUpload.value = true
  }

  function uploadFiles(event: Event): void {
    const input = event.target as HTMLInputElement
    try {
      if (disposed || !context.requireUpload()) return
      const candidates = candidatesFromFiles(input.files ?? [])
      if (candidates.length) void queueUploads(candidates)
    } catch (error) {
      announceSelectionError(error)
    } finally {
      input.value = ''
    }
  }

  function announceSelectionError(error: unknown): void {
    if (disposed) return
    context.announce(error instanceof UploadSelectionLimitError
      ? locale.text(`一次最多选择 ${MAX_UPLOAD_QUEUE_TASKS} 个文件及目录，请分批上传`, `Select at most ${MAX_UPLOAD_QUEUE_TASKS} files and folders at a time; split the upload`)
      : error instanceof Error ? error.message : locale.text('无法读取上传内容', 'Unable to read the selected items'))
  }

  function uploadLimitError(file: File): string {
    if (context.maxUploadBytes.value && file.size > context.maxUploadBytes.value) {
      return locale.text(
        `文件超过单文件上传上限 ${formatSize(context.maxUploadBytes.value)}`,
        `File exceeds the ${formatSize(context.maxUploadBytes.value)} per-file limit`,
      )
    }
    return ''
  }

  function batchLimitError(tasks: UploadTask[]): string {
    const bytes = tasks.reduce((total, task) => total + task.file.size, 0)
    if (context.maxUploadBatchEntries.value && tasks.length > context.maxUploadBatchEntries.value) {
      return locale.text(
        `本批次最多上传 ${context.maxUploadBatchEntries.value} 个文件`,
        `This batch is limited to ${context.maxUploadBatchEntries.value} files`,
      )
    }
    if (context.maxUploadBatchBytes.value && bytes > context.maxUploadBatchBytes.value) {
      return locale.text(
        `本批次总大小不能超过 ${formatSize(context.maxUploadBatchBytes.value)}`,
        `This batch cannot exceed ${formatSize(context.maxUploadBatchBytes.value)}`,
      )
    }
    return ''
  }

  async function queueUploads(candidates: UploadCandidate[], destination: UploadDestination = { storageId: context.storageId.value, basePath: context.path.value }): Promise<void> {
    if (disposed || !candidates.length || !context.requireUpload()) return
    const { storageId, basePath } = destination
    const occupied = new Set(uploadTasks.value.filter(task => task.storageId === storageId
      && isUploadPathReserved(task))
      .map(task => task.originalTargetPath ?? task.targetPath))
    const unique = candidates.filter(candidate => {
      const target = joinUploadPath(basePath, candidate.relativePath)
      if (occupied.has(target)) return false
      occupied.add(target)
      return true
    })
    if (unique.length !== candidates.length) context.announce(locale.text('已忽略重复排队的文件，请等待当前任务完成', 'Duplicate queued files were ignored; wait for the current task to finish'))
    if (uploadTasks.value.length + unique.length > MAX_UPLOAD_QUEUE_TASKS) {
      context.announce(locale.text('上传队列已满，请清除已完成记录或分批上传', 'Upload queue is full; clear completed records or split the upload'))
      return
    }
    const addedTasks: UploadTask[] = unique.map(candidate => {
      const error = uploadLimitError(candidate.file)
      return {
        ...candidate,
        id: ++uploadTaskSequence,
        storageId,
        basePath,
        targetPath: joinUploadPath(basePath, candidate.relativePath),
        originalTargetPath: joinUploadPath(basePath, candidate.relativePath),
        status: error ? 'failed' : 'queued',
        loaded: 0,
        error,
      }
    })
    uploadTasks.value.push(...addedTasks)
    const rejected = addedTasks.find(task => task.error)
    if (rejected) context.announce(rejected.error)
    showUpload.value = true
    if (!uploading.value) {
      await runUploadTasks(addedTasks.filter(task => task.status === 'queued').map(task => task.id))
    }
  }

  async function verifyExistingTickets(tasks: UploadTask[]): Promise<void> {
    // Only definitely unstarted or confirmed-uncommitted files may receive a
    // fresh reservation after an old ticket expires.
    const checkedTickets = new Map<string, Map<string, UploadBatchStatus['items'][number]> | Error>()
    for (const task of tasks) {
      if (disposed) return
      if (!task.ticket) continue
      const key = uploadTicketKey(task.storageId, task.ticket)
      if (!checkedTickets.has(key)) {
        try {
          const batch = await getUploadBatchStatus(task.ticket, task.storageId)
          if (disposed) return
          checkedTickets.set(key, indexUploadBatchItems(batch))
        } catch (error) {
          if (disposed) return
          checkedTickets.set(key, error instanceof Error ? error : new Error(String(error)))
        }
      }
      const checked = checkedTickets.get(key)!
      if (checked instanceof Error) {
        if (checked instanceof ApiError && checked.code === 'upload_batch_expired' && (!task.attempted || task.safeToPrepare)) {
          task.ticket = undefined
          task.targetPath = task.originalTargetPath ?? task.targetPath
          task.relativePath = task.basePath && task.targetPath.startsWith(task.basePath + '/')
            ? task.targetPath.slice(task.basePath.length + 1) : task.targetPath
        } else if (task.attempted && !task.safeToPrepare) {
          markUnconfirmed(task)
        } else {
          task.status = 'failed'
          task.error = checked.message
        }
        continue
      }
      const item = checked.get(task.targetPath)
      if (!item) { markUnconfirmed(task); continue }
      if (item.status !== 'pending' && item.status !== 'failed') {
        applyServerUploadState(task, item.status, item.operation)
        if (item.status === 'in_progress' || item.status === 'unknown') void reconciliation.reconcile(task)
      }
    }
  }

  async function prepareMissingTickets(tasks: UploadTask[]): Promise<void> {
    const prepareGroups = groupUploadTasksByStorage(tasks.filter(task => task.status === 'queued' && !task.ticket))
    for (const [storageId, group] of prepareGroups) {
      if (disposed) return
      const limitError = batchLimitError(group)
      if (limitError) {
        for (const task of group) {
          task.status = 'failed'
          task.error = limitError
        }
        continue
      }
      for (const task of group) task.status = 'preparing'
      try {
        const prepared = await prepareUploadBatch(
          group.map(task => ({ path: task.targetPath, size: task.file.size })),
          storageId,
        )
        if (disposed) {
          await cancelUploadBatch(prepared.ticket, storageId).catch(() => undefined)
          return
        }
        const assignedPaths = new Map<string, string>()
        for (const item of prepared.items ?? []) {
          const originalPath = item.original_path
          if (!assignedPaths.has(originalPath)) assignedPaths.set(originalPath, item.path)
        }
        for (const task of group) {
          const assignedPath = assignedPaths.get(task.targetPath)
          if (assignedPath !== undefined) {
            task.targetPath = assignedPath
            task.relativePath = task.basePath && assignedPath.startsWith(task.basePath + '/')
              ? assignedPath.slice(task.basePath.length + 1) : assignedPath
          }
        }
        const cancelledPaths = group
          .filter(task => task.status === 'cancelled')
          .map(task => task.targetPath)
        if (cancelledPaths.length) {
          await cancelUploadBatch(prepared.ticket, storageId, cancelledPaths).catch(() => undefined)
        }
        if (disposed) {
          await cancelUploadBatch(prepared.ticket, storageId).catch(() => undefined)
          return
        }
        for (const task of group) {
          if (task.status === 'cancelled') continue
          task.ticket = prepared.ticket
          task.directUpload = prepared.upload_mode === 'direct'
          if (task.status === 'preparing') task.status = 'queued'
        }
      } catch (error) {
        if (disposed) return
        const message = error instanceof Error ? error.message : locale.text('无法开始上传', 'Unable to start the upload')
        for (const task of group) {
          if (task.status !== 'preparing') continue
          task.status = 'failed'
          task.error = message
        }
        context.announce(message)
      }
    }
  }

  async function uploadPreparedTasks(tasks: UploadTask[], completedContexts: Set<string>): Promise<void> {
    const groups = groupUploadTasksByTicket(tasks.filter(task => task.status === 'queued'))
    for (const { tasks: group, ticket, storageId } of groups.values()) {
      if (!storageId) continue
      for (const task of group) {
        if (disposed) return
        if (task.status !== 'queued' || task.ticket !== ticket || task.storageId !== storageId) continue
        task.status = 'uploading'
        task.loaded = 0
        task.error = ''
        try {
          currentUploadTaskId = task.id
          currentUploadController = new AbortController()
          task.attempted = true
          task.safeToPrepare = false
          await uploadFile(task.targetPath, task.file, loaded => { task.loaded = loaded }, task.storageId, ticket, currentUploadController.signal, task.directUpload)
          if (disposed) return
          if (task.cancelRequested || task.pauseRequested) {
            await reconcileInterruptedUpload(task)
          } else {
            task.loaded = task.file.size
            task.status = 'succeeded'
            completedContexts.add(`${task.storageId}\u0000${task.basePath}`)
          }
        } catch (error) {
          if (disposed) return
          if (task.cancelRequested || task.pauseRequested) {
            task.loaded = 0
            await reconcileInterruptedUpload(task)
          } else {
            task.error = error instanceof Error ? error.message : locale.text('上传异常', 'Upload error')
            if (error instanceof ApiError && error.blocksRetry) {
              task.status = 'verifying'
              task.retryBlocked = true
              void reconciliation.reconcile(task)
            } else {
              task.status = 'failed'
              task.retryBlocked = false
              task.safeToPrepare = error instanceof ApiError && (error.code === 'upload_batch_expired' || error.operation?.commit === 'not_committed')
              context.announce(task.error)
            }
          }
        } finally {
          currentUploadController = undefined
          currentUploadTaskId = undefined
        }
      }
    }
  }

  async function runUploadTasks(taskIds: number[]): Promise<void> {
    if (disposed || !taskIds.length || uploading.value) return
    const selectedIds = new Set(taskIds)
    const tasks = uploadTasks.value.filter(task => selectedIds.has(task.id) && task.status === 'queued')
    if (!tasks.length) return
    uploading.value = true
    const completedContexts = new Set<string>()
    try {
      // A fresh queue must enter "preparing" synchronously, as it did before
      // this phase was extracted; avoid an extra await when no ticket exists.
      if (tasks.some(task => task.ticket)) await verifyExistingTickets(tasks)
      if (disposed) return
      if (tasks.some(task => task.status === 'queued' && !task.ticket)) await prepareMissingTickets(tasks)
      if (disposed) return
      if (tasks.some(task => task.status === 'queued' && task.ticket)) await uploadPreparedTasks(tasks, completedContexts)
    } catch (error) {
      if (disposed) return
      const message = error instanceof Error ? error.message : locale.text('无法开始上传', 'Unable to start the upload')
      for (const task of tasks.filter(task => task.status === 'preparing' || task.status === 'queued')) {
        task.status = 'failed'
        task.error = message
      }
      context.announce(message)
    } finally {
      uploading.value = false
      if (!disposed) {
        if (completedContexts.has(`${context.storageId.value}\u0000${context.path.value}`)) requestContextRefresh()
        const pending = uploadTasks.value.filter(task => task.status === 'queued').map(task => task.id)
        if (pending.length) void runUploadTasks(pending)
      }
    }
  }

  function retryUpload(id: number): void {
    if (disposed) return
    const task = uploadTasks.value.find(item => item.id === id && item.status === 'failed')
    if (!task) return
    if (!canUploadTaskAction(task, 'retry')) {
      context.announce(task.error)
      return
    }
    const error = uploadLimitError(task.file)
    task.status = error ? 'failed' : 'queued'
    task.loaded = 0
    task.error = error
    task.cancelRequested = false
    task.pauseRequested = false
    if (!error) void runUploadTasks([id])
  }

  function removeFailedUpload(id: number): void {
    if (disposed) return
    const removed = uploadTasks.value.find(task => task.id === id && task.status === 'failed')
    uploadTasks.value = uploadTasks.value.filter(task => task !== removed)
    if (removed) releaseUnusedUploadTickets([removed])
  }

  function closeUploadDialog(): void {
    showUpload.value = false
  }

  function pauseUploads(taskIds: number[]): void {
    if (disposed) return
    const selectedIds = new Set(taskIds)
    for (const task of uploadTasks.value) {
      if (!selectedIds.has(task.id) || !canUploadTaskAction(task, 'pause')) continue
      if (task.status === 'preparing' || task.status === 'queued') {
        task.pauseRequested = true
        task.status = 'paused'
        continue
      }
      if (task.status === 'uploading') {
        task.pauseRequested = true
        task.status = 'verifying'
        task.error = locale.text('正在确认暂停后的实际结果', 'Checking the result after pausing')
        if (currentUploadTaskId === task.id) currentUploadController?.abort()
      }
    }
  }

  function resumeUploads(taskIds: number[]): void {
    if (disposed) return
    const selectedIds = new Set(taskIds)
    const ids: number[] = []
    for (const task of uploadTasks.value) {
      if (!selectedIds.has(task.id) || !canUploadTaskAction(task, 'resume')) continue
      task.status = 'queued'
      task.error = ''
      task.pauseRequested = false
      ids.push(task.id)
    }
    if (!uploading.value) void runUploadTasks(ids)
  }

  function terminateUploads(taskIds: number[]): void {
    if (disposed) return
    const selectedIds = new Set(taskIds)
    const selectedTasks = uploadTasks.value.filter(task => selectedIds.has(task.id) && canUploadTaskAction(task, 'terminate'))
    if (!selectedTasks.length) return
    for (const task of selectedTasks) {
      task.cancelRequested = true
      task.pauseRequested = false
      task.status = task.status === 'uploading' ? 'verifying' : 'cancelled'
      task.loaded = 0
      task.error = task.status === 'verifying'
        ? locale.text('正在确认终止后的实际结果', 'Checking the result after termination')
        : locale.text('任务已终止', 'Task terminated')
    }
    if (selectedTasks.some(task => task.id === currentUploadTaskId)) {
      currentUploadController?.abort()
    }
    cancelPendingUploadItems(selectedTasks)
  }

  function clearUploadTasks(taskIds: number[]): void {
    if (disposed) return
    const selectedIds = new Set(taskIds)
    const removedTasks = uploadTasks.value.filter(task => selectedIds.has(task.id))
    if (!removedTasks.length || removedTasks.some(task => !canUploadTaskAction(task, 'clear'))) return
    uploadTasks.value = uploadTasks.value.filter(task => !selectedIds.has(task.id))
    releaseUnusedUploadTickets(removedTasks)
  }

  function releaseUnusedUploadTickets(tasks: UploadTask[]): void {
    const neededTickets = new Set<string>()
    for (const task of uploadTasks.value) {
      if (task.ticket && task.status !== 'succeeded' && task.status !== 'cancelled') {
        neededTickets.add(uploadTicketKey(task.storageId, task.ticket))
      }
    }
    for (const [key, { ticket, storageId }] of groupUploadTasksByTicket(tasks)) {
      if (!neededTickets.has(key)) void cancelUploadBatch(ticket, storageId).catch(() => undefined)
    }
  }

  function cancelPendingUploadItems(tasks: UploadTask[]): void {
    const groups = groupUploadTasksByTicket(tasks.filter(task => task.status !== 'verifying'))
    for (const { ticket, storageId, tasks: group } of groups.values()) {
      void cancelUploadBatch(ticket, storageId, group.map(task => task.targetPath)).catch(() => undefined)
    }
  }

  async function requestCancellationAndReconcile(task: UploadTask): Promise<void> {
    if (disposed) return
    if (!task.ticket) {
      markUnconfirmed(task)
      return
    }
    await cancelUploadBatch(task.ticket, task.storageId, [task.targetPath]).catch(() => undefined)
    await reconciliation.reconcile(task)
  }

  async function reconcileInterruptedUpload(task: UploadTask): Promise<void> {
    task.status = 'verifying'
    task.error = task.cancelRequested
      ? locale.text('正在确认终止后的实际结果', 'Checking the result after termination')
      : locale.text('正在确认暂停后的实际结果', 'Checking the result after pausing')
    if (task.cancelRequested) await requestCancellationAndReconcile(task)
    else await reconciliation.reconcile(task)
  }

  function applyServerUploadState(task: UploadTask, status: UploadBatchItemState, operation?: ApiError['operation']): void {
    if (disposed) return
    applyUploadServerState(task, status, operation, locale.text)
    if (status === 'complete') refreshTaskContext(task)
    if (status === 'unknown') context.announce(task.error)
  }

  function markUnconfirmed(task: UploadTask): void {
    if (disposed) return
    markUploadUnconfirmed(task, locale.text)
    context.announce(task.error)
  }

  function refreshTaskContext(task: UploadTask): void {
    if (!disposed && task.storageId === context.storageId.value && task.basePath === context.path.value) {
      requestContextRefresh()
    }
  }

  function requestContextRefresh(): void {
    if (disposed) return
    pendingRefresh = { storageId: context.storageId.value, basePath: context.path.value }
    if (!refreshing) void refreshUploadContext()
  }

  async function refreshUploadContext(): Promise<void> {
    refreshing = true
    try {
      while (!disposed && pendingRefresh) {
        const destination = pendingRefresh
        pendingRefresh = undefined
        if (destination.storageId !== context.storageId.value || destination.basePath !== context.path.value) continue
        try { await context.refresh() }
        catch (error) {
          if (!disposed && destination.storageId === context.storageId.value && destination.basePath === context.path.value) {
            context.announce(error instanceof Error ? error.message : locale.text('目录加载失败', 'Unable to load this folder'))
          }
        }
      }
    } finally {
      refreshing = false
    }
  }

  async function handleUploadDrop(event: DragEvent): Promise<void> {
    uploadDragDepth = 0
    uploadDropActive.value = false
    const transfer = event.dataTransfer
    if (disposed || !transfer || !context.requireUpload()) return
    const destination = { storageId: context.storageId.value, basePath: context.path.value }
    const controller = new AbortController()
    selectionControllers.add(controller)
    try {
      const candidates = await candidatesFromDrop(transfer, { signal: controller.signal })
      await queueUploads(candidates, destination)
    } catch (error) {
      announceSelectionError(error)
    } finally {
      selectionControllers.delete(controller)
    }
  }

  function blockExternalFileDrop(event: DragEvent): void {
    if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
    if (filePanel.value?.contains(event.target as Node)) return
    uploadDragDepth = 0
    uploadDropActive.value = false
    event.preventDefault()
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'none'
  }

  function handleUploadDragEnter(event: DragEvent): void {
    if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
    event.preventDefault()
    event.stopPropagation()
    uploadDragDepth += 1
    uploadDropActive.value = context.canUpload()
    if (event.dataTransfer) event.dataTransfer.dropEffect = context.canUpload() ? 'copy' : 'none'
  }

  function handleUploadDragOver(event: DragEvent): void {
    if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
    event.preventDefault()
    event.stopPropagation()
    if (event.dataTransfer) event.dataTransfer.dropEffect = context.canUpload() ? 'copy' : 'none'
  }

  function handleUploadDragLeave(event: DragEvent): void {
    if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
    event.preventDefault()
    event.stopPropagation()
    uploadDragDepth = Math.max(0, uploadDragDepth - 1)
    if (!uploadDragDepth) uploadDropActive.value = false
  }

  function disposeUploads(): void {
    disposed = true
    for (const controller of selectionControllers) controller.abort()
    selectionControllers.clear()
    currentUploadController?.abort()
    reconciliation.dispose()
    pendingRefresh = undefined
    for (const { ticket, storageId } of groupUploadTasksByTicket(uploadTasks.value).values()) {
      void cancelUploadBatch(ticket, storageId).catch(() => undefined)
    }
  }

  return {
    blockExternalFileDrop,
    chooseFiles,
    chooseFolder,
    clearUploadTasks,
    closeUploadDialog,
    disposeUploads,
    filePanel,
    handleUploadDragEnter,
    handleUploadDragLeave,
    handleUploadDragOver,
    handleUploadDrop,
    openUploadManager,
    pauseUploads,
    removeFailedUpload,
    resumeUploads,
    retryUpload,
    setFileInput,
    setFolderInput,
    showUpload,
    terminateUploads,
    uploadDropActive,
    uploadFiles,
    uploadTasks,
  }
}

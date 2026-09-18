import { ref } from 'vue'
import type { Ref } from 'vue'
import { cancelUploadBatch, getUploadBatchStatus, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import type { UploadBatchItemState } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import { candidatesFromDrop, candidatesFromFiles, joinUploadPath } from './uploadQueue'
import type { UploadCandidate, UploadTask } from './uploadQueue'
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

export function useUploadQueue(context: UploadQueueContext) {
  const locale = useLocale()
  const fileInput = ref<HTMLInputElement>()
  const folderInput = ref<HTMLInputElement>()
  const filePanel = ref<HTMLElement>()
  const showUpload = ref(false)
  const uploading = ref(false)
  const uploadTasks = ref<UploadTask[]>([])
  const uploadDropActive = ref(false)
  let uploadTaskSequence = 0
  let uploadDragDepth = 0
  let currentUploadController: AbortController | undefined
  let currentUploadTaskId: number | undefined
  let disposed = false
  const reconcileTimers = new Set<number>()
  const reconcileDelays = [0, 1000, 3000, 7000, 15000, 30000]

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
    const candidates = candidatesFromFiles(input.files ?? [])
    input.value = ''
    if (candidates.length) void queueUploads(candidates)
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

  async function queueUploads(candidates: UploadCandidate[]): Promise<void> {
    if (!candidates.length || !context.requireUpload()) return
    const storageId = context.storageId.value
    const basePath = context.path.value
    const addedTasks: UploadTask[] = candidates.map(candidate => {
      const error = uploadLimitError(candidate.file)
      return {
        ...candidate,
        id: ++uploadTaskSequence,
        storageId,
        basePath,
        targetPath: joinUploadPath(basePath, candidate.relativePath),
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

  async function runUploadTasks(taskIds: number[]): Promise<void> {
    if (!taskIds.length || uploading.value) return
    const selectedIds = new Set(taskIds)
    const tasks = uploadTasks.value.filter(task => selectedIds.has(task.id) && task.status === 'queued')
    if (!tasks.length) return
    uploading.value = true
    const completedContexts = new Set<string>()
    try {
      const tasksWithoutTicket = tasks.filter(task => !task.ticket)
      const prepareGroups = new Map<string, UploadTask[]>()
      for (const task of tasksWithoutTicket) {
        prepareGroups.set(task.storageId, [...(prepareGroups.get(task.storageId) ?? []), task])
      }
      for (const [storageId, group] of prepareGroups) {
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
            for (const task of group) {
              if (task.status === 'preparing') task.status = 'queued'
            }
            continue
          }
          const cancelledPaths = group
            .filter(task => task.status === 'cancelled')
            .map(task => task.targetPath)
          if (cancelledPaths.length) {
            await cancelUploadBatch(prepared.ticket, storageId, cancelledPaths).catch(() => undefined)
          }
          for (const task of group) {
            if (task.status === 'cancelled') continue
            task.ticket = prepared.ticket
            if (task.status === 'preparing') task.status = 'queued'
          }
        } catch (error) {
          const message = error instanceof Error ? error.message : locale.text('无法开始上传', 'Unable to start the upload')
          for (const task of group) {
            if (task.status !== 'preparing') continue
            task.status = 'failed'
            task.error = message
          }
          context.announce(message)
        }
      }
      const groups = new Map<string, UploadTask[]>()
      for (const task of tasks) {
        if (task.status !== 'queued' || !task.ticket) continue
        const key = `${task.storageId}\u0000${task.ticket}`
        groups.set(key, [...(groups.get(key) ?? []), task])
      }
      for (const group of groups.values()) {
        const ticket = group[0]?.ticket
        const storageId = group[0]?.storageId
        if (!ticket || !storageId) continue
        for (const task of group) {
          if (task.status !== 'queued' || task.ticket !== ticket || task.storageId !== storageId) continue
          task.status = 'uploading'
          task.loaded = 0
          task.error = ''
          try {
            currentUploadTaskId = task.id
            currentUploadController = new AbortController()
            await uploadFile(task.targetPath, task.file, loaded => { task.loaded = loaded }, task.storageId, ticket, currentUploadController.signal)
            if (task.cancelRequested) {
              task.status = 'verifying'
              task.error = locale.text('正在确认终止后的实际结果', 'Checking the result after termination')
              await requestCancellationAndReconcile(task)
            } else if (task.pauseRequested) {
              task.status = 'verifying'
              task.error = locale.text('正在确认暂停后的实际结果', 'Checking the result after pausing')
              await reconcileUploadTask(task)
            } else {
              task.loaded = task.file.size
              task.status = 'succeeded'
              completedContexts.add(`${task.storageId}\u0000${task.basePath}`)
            }
          } catch (error) {
            if (task.cancelRequested) {
              task.status = 'verifying'
              task.loaded = 0
              task.error = locale.text('正在确认终止后的实际结果', 'Checking the result after termination')
              await requestCancellationAndReconcile(task)
            } else if (task.pauseRequested) {
              task.status = 'verifying'
              task.loaded = 0
              task.error = locale.text('正在确认暂停后的实际结果', 'Checking the result after pausing')
              await reconcileUploadTask(task)
            } else {
              task.error = error instanceof Error ? error.message : locale.text('上传异常', 'Upload error')
              if (error instanceof ApiError && error.blocksRetry) {
                task.status = 'verifying'
                task.retryBlocked = true
                void reconcileUploadTask(task)
              } else {
                task.status = 'failed'
                task.retryBlocked = false
                context.announce(task.error)
              }
            }
          } finally {
            currentUploadController = undefined
            currentUploadTaskId = undefined
          }
        }
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : locale.text('无法开始上传', 'Unable to start the upload')
      for (const task of tasks.filter(task => task.status === 'preparing' || task.status === 'queued')) {
        task.status = 'failed'
        task.error = message
      }
      context.announce(message)
    } finally {
      uploading.value = false
      if (!disposed) {
        if (completedContexts.has(`${context.storageId.value}\u0000${context.path.value}`)) await context.refresh()
        const pending = uploadTasks.value.filter(task => task.status === 'queued').map(task => task.id)
        if (pending.length) void runUploadTasks(pending)
      }
    }
  }

  function retryUpload(id: number): void {
    const task = uploadTasks.value.find(item => item.id === id && item.status === 'failed')
    if (!task) return
    if (task.retryBlocked) {
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
    const removed = uploadTasks.value.find(task => task.id === id && task.status === 'failed')
    uploadTasks.value = uploadTasks.value.filter(task => task !== removed)
    if (removed) releaseUnusedUploadTickets([removed])
  }

  function closeUploadDialog(): void {
    showUpload.value = false
  }

  function pauseUploads(taskIds: number[]): void {
    const selectedIds = new Set(taskIds)
    for (const task of uploadTasks.value) {
      if (!selectedIds.has(task.id)) continue
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
    const selectedIds = new Set(taskIds)
    const ids: number[] = []
    for (const task of uploadTasks.value) {
      if (!selectedIds.has(task.id) || task.status !== 'paused') continue
      task.status = 'queued'
      task.error = ''
      task.pauseRequested = false
      ids.push(task.id)
    }
    if (!uploading.value) void runUploadTasks(ids)
  }

  function terminateUploads(taskIds: number[]): void {
    const selectedIds = new Set(taskIds)
    const selectedTasks = uploadTasks.value.filter(task => selectedIds.has(task.id))
    if (!selectedTasks.some(task => ['preparing', 'queued', 'uploading', 'paused'].includes(task.status))) return
    for (const task of selectedTasks) {
      if (!['preparing', 'queued', 'uploading', 'paused'].includes(task.status)) continue
      task.cancelRequested = true
      task.pauseRequested = false
      task.status = task.status === 'uploading' ? 'verifying' : 'cancelled'
      task.loaded = 0
      task.error = task.status === 'verifying'
        ? locale.text('正在确认终止后的实际结果', 'Checking the result after termination')
        : locale.text('任务已终止', 'Task terminated')
    }
    if (currentUploadTaskId !== undefined && selectedIds.has(currentUploadTaskId)) {
      currentUploadController?.abort()
    }
    cancelPendingUploadItems(selectedTasks)
  }

  function clearUploadTasks(taskIds: number[]): void {
    const selectedIds = new Set(taskIds)
    const removedTasks = uploadTasks.value.filter(task => selectedIds.has(task.id))
    if (removedTasks.some(task => ['preparing', 'queued', 'uploading', 'paused', 'verifying'].includes(task.status))) return
    uploadTasks.value = uploadTasks.value.filter(task => !selectedIds.has(task.id))
    releaseUnusedUploadTickets(removedTasks)
  }

  function releaseUnusedUploadTickets(tasks: UploadTask[]): void {
    const tickets = new Map<string, { ticket: string; storageId: string }>()
    for (const task of tasks) {
      if (!task.ticket) continue
      tickets.set(`${task.storageId}\u0000${task.ticket}`, { ticket: task.ticket, storageId: task.storageId })
    }
    for (const { ticket, storageId } of tickets.values()) {
      const stillNeeded = uploadTasks.value.some(task => task.storageId === storageId && task.ticket === ticket && task.status !== 'succeeded' && task.status !== 'cancelled')
      if (!stillNeeded) void cancelUploadBatch(ticket, storageId).catch(() => undefined)
    }
  }

  function cancelPendingUploadItems(tasks: UploadTask[]): void {
    const tickets = new Map<string, { ticket: string; storageId: string; paths: string[] }>()
    for (const task of tasks) {
      if (!task.ticket || task.status === 'verifying') continue
      const key = `${task.storageId}\u0000${task.ticket}`
      const group = tickets.get(key) ?? { ticket: task.ticket, storageId: task.storageId, paths: [] }
      group.paths.push(task.targetPath)
      tickets.set(key, group)
    }
    for (const { ticket, storageId, paths } of tickets.values()) {
      void cancelUploadBatch(ticket, storageId, paths).catch(() => undefined)
    }
  }

  async function requestCancellationAndReconcile(task: UploadTask): Promise<void> {
    if (!task.ticket) {
      markUnconfirmed(task)
      return
    }
    await cancelUploadBatch(task.ticket, task.storageId, [task.targetPath]).catch(() => undefined)
    await reconcileUploadTask(task)
  }

  async function reconcileUploadTask(task: UploadTask, attempt = 0): Promise<void> {
    if (disposed || !task.ticket || !uploadTasks.value.includes(task)) return
    try {
      const batch = await getUploadBatchStatus(task.ticket, task.storageId)
      const item = batch.items.find(candidate => candidate.path === task.targetPath)
      if (!item) {
        scheduleReconcile(task, attempt)
        return
      }
      applyServerUploadState(task, item.status, item.operation)
      if (item.status === 'in_progress') scheduleReconcile(task, attempt)
    } catch {
      scheduleReconcile(task, attempt)
    }
  }

  function scheduleReconcile(task: UploadTask, attempt: number): void {
    const delay = reconcileDelays[attempt]
    if (delay === undefined) {
      markUnconfirmed(task)
      return
    }
    const timer = window.setTimeout(() => {
      reconcileTimers.delete(timer)
      void reconcileUploadTask(task, attempt + 1)
    }, delay)
    reconcileTimers.add(timer)
  }

  function applyServerUploadState(task: UploadTask, status: UploadBatchItemState, operation?: ApiError['operation']): void {
    task.retryBlocked = false
    if (status === 'complete') {
      task.loaded = task.file.size
      if (operation) {
        task.status = 'failed'
        task.retryBlocked = true
        task.error = locale.text('文件变更已提交，请勿重复上传；可稍后核对清理状态', 'The file change was committed. Do not upload it again; check cleanup status later.')
      } else {
        task.status = 'succeeded'
        task.error = ''
      }
      task.cancelRequested = false
      task.pauseRequested = false
      refreshTaskContext(task)
      return
    }
    if (status === 'unknown') {
      markUnconfirmed(task)
      return
    }
    if (status === 'in_progress') {
      task.status = 'verifying'
      task.retryBlocked = true
      task.error = locale.text('操作仍在服务端执行，正在确认结果', 'The operation is still running on the server')
      return
    }
    if (status === 'cancelled' || (status === 'failed' && task.cancelRequested)) {
      task.status = 'cancelled'
      task.loaded = 0
      task.error = locale.text('任务已终止，服务端确认未提交', 'Task terminated; the server confirmed it was not committed')
      task.cancelRequested = false
      task.pauseRequested = false
      return
    }
    if ((status === 'pending' || status === 'failed') && task.pauseRequested) {
      task.status = 'paused'
      task.loaded = 0
      task.error = ''
      task.pauseRequested = false
      return
    }
    task.status = 'failed'
    task.loaded = 0
    task.error = status === 'pending'
      ? locale.text('服务端确认上传尚未开始，可以重试', 'The server confirmed the upload did not start; it can be retried')
      : locale.text('服务端确认上传失败，可以重试', 'The server confirmed the upload failed; it can be retried')
    task.cancelRequested = false
    task.pauseRequested = false
  }

  function markUnconfirmed(task: UploadTask): void {
    task.status = 'failed'
    task.retryBlocked = true
    task.error = locale.text('上传结果无法确认，请核对文件后再操作，不要直接重试', 'The upload result could not be confirmed. Check the file before retrying.')
    context.announce(task.error)
  }

  function refreshTaskContext(task: UploadTask): void {
    if (task.storageId === context.storageId.value && task.basePath === context.path.value) {
      void context.refresh()
    }
  }

  async function handleUploadDrop(event: DragEvent): Promise<void> {
    uploadDragDepth = 0
    uploadDropActive.value = false
    const transfer = event.dataTransfer
    if (!transfer || !context.requireUpload()) return
    try {
      await queueUploads(await candidatesFromDrop(transfer))
    } catch (error) {
      context.announce(error instanceof Error ? error.message : locale.text('无法读取拖放内容', 'Unable to read the dropped items'))
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
    currentUploadController?.abort()
    for (const timer of reconcileTimers) window.clearTimeout(timer)
    reconcileTimers.clear()
    const tickets = new Map<string, { ticket: string; storageId: string }>()
    for (const task of uploadTasks.value) {
      if (!task.ticket) continue
      tickets.set(`${task.storageId}\u0000${task.ticket}`, { ticket: task.ticket, storageId: task.storageId })
    }
    for (const { ticket, storageId } of tickets.values()) {
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

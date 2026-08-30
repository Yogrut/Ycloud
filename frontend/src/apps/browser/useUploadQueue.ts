import { ref } from 'vue'
import type { Ref } from 'vue'
import { cancelUploadBatch, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import { candidatesFromDrop, candidatesFromFiles, joinUploadPath } from './uploadQueue'
import type { UploadCandidate, UploadTask } from './uploadQueue'

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
  const cancelledUploadTaskIds = new Set<number>()
  const requeuedUploadTaskIds = new Set<number>()

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
          if (disposed || group.some(task => task.status === 'cancelled')) {
            await cancelUploadBatch(prepared.ticket, storageId).catch(() => undefined)
            for (const task of group) {
              if (task.status === 'preparing') task.status = 'queued'
            }
            continue
          }
          for (const task of group) {
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
            if (!cancelledUploadTaskIds.has(task.id)) {
              task.loaded = task.file.size
              task.status = 'succeeded'
              completedContexts.add(`${task.storageId}\u0000${task.basePath}`)
            }
          } catch (error) {
            if (cancelledUploadTaskIds.has(task.id)) {
              task.status = 'cancelled'
              task.loaded = 0
              task.error = locale.text('任务已终止', 'Task terminated')
            } else if (requeuedUploadTaskIds.has(task.id)) {
              task.status = 'queued'
              task.loaded = 0
              task.error = ''
            } else {
              task.status = 'failed'
              task.error = error instanceof Error ? error.message : locale.text('上传异常', 'Upload error')
            }
          } finally {
            currentUploadController = undefined
            currentUploadTaskId = undefined
            cancelledUploadTaskIds.delete(task.id)
            requeuedUploadTaskIds.delete(task.id)
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
    const error = uploadLimitError(task.file)
    task.status = error ? 'failed' : 'queued'
    task.loaded = 0
    task.error = error
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
      if (selectedIds.has(task.id) && task.status === 'queued') task.status = 'paused'
    }
  }

  function resumeUploads(taskIds: number[]): void {
    const selectedIds = new Set(taskIds)
    const ids: number[] = []
    for (const task of uploadTasks.value) {
      if (!selectedIds.has(task.id) || task.status !== 'paused') continue
      task.status = 'queued'
      task.error = ''
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
      task.status = 'cancelled'
      task.loaded = 0
      task.error = locale.text('任务已终止', 'Task terminated')
    }
    if (currentUploadTaskId !== undefined && selectedIds.has(currentUploadTaskId)) {
      cancelledUploadTaskIds.add(currentUploadTaskId)
      currentUploadController?.abort()
    }
    invalidateSelectedUploadTickets(selectedTasks)
  }

  function clearUploadTasks(taskIds: number[]): void {
    const selectedIds = new Set(taskIds)
    const removedTasks = uploadTasks.value.filter(task => selectedIds.has(task.id))
    if (removedTasks.some(task => ['preparing', 'queued', 'uploading', 'paused'].includes(task.status))) return
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

  function invalidateSelectedUploadTickets(tasks: UploadTask[]): void {
    const tickets = new Map<string, { ticket: string; storageId: string }>()
    for (const task of tasks) {
      if (!task.ticket) continue
      tickets.set(`${task.storageId}\u0000${task.ticket}`, { ticket: task.ticket, storageId: task.storageId })
    }
    for (const { ticket, storageId } of tickets.values()) {
      for (const task of uploadTasks.value) {
        if (task.storageId !== storageId || task.ticket !== ticket) continue
        task.ticket = undefined
        if (task.status === 'uploading') {
          requeuedUploadTaskIds.add(task.id)
          if (currentUploadTaskId === task.id) currentUploadController?.abort()
        }
      }
      void cancelUploadBatch(ticket, storageId).catch(() => undefined)
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

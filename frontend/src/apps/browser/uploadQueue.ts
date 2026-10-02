import type { UploadCandidate } from './uploadCandidates'

export type UploadTaskStatus = 'preparing' | 'queued' | 'uploading' | 'paused' | 'verifying' | 'succeeded' | 'failed' | 'cancelled'

export interface UploadTask extends UploadCandidate {
  id: number
  storageId: string
  basePath: string
  targetPath: string
  originalTargetPath?: string
  attempted?: boolean
  safeToPrepare?: boolean
  ticket?: string
  directUpload?: boolean
  status: UploadTaskStatus
  loaded: number
  error: string
  retryBlocked?: boolean
  cancelRequested?: boolean
  pauseRequested?: boolean
}

export type UploadTaskAction = 'pause' | 'resume' | 'terminate' | 'retry' | 'clear'

export function isUploadTaskActive(task: UploadTask): boolean {
  return task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading'
    || task.status === 'paused' || task.status === 'verifying'
}

export function canUploadTaskAction(task: UploadTask, action: UploadTaskAction): boolean {
  switch (action) {
    case 'pause': return task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading'
    case 'resume': return task.status === 'paused'
    case 'terminate': return canUploadTaskAction(task, 'pause') || task.status === 'paused'
    case 'retry': return task.status === 'failed' && !task.retryBlocked
    // Clearing a terminal record is not deleting the uploaded file or
    // declaring an uncertain server operation uncommitted.
    case 'clear': return !isUploadTaskActive(task)
  }
}

export function isUploadPathReserved(task: UploadTask): boolean {
  return isUploadTaskActive(task) || task.retryBlocked === true
}

export function uploadLoadedBytes(task: UploadTask): number {
  return Number.isNaN(task.loaded) ? 0 : Math.max(0, Math.min(task.file.size, task.loaded))
}

export function uploadTaskPercent(task: UploadTask): number {
  if (task.status === 'succeeded') return 100
  if (!task.file.size || (task.status !== 'uploading' && task.status !== 'verifying')) return 0
  return Math.min(99, Math.round(uploadLoadedBytes(task) / task.file.size * 100))
}

export function summarizeUploadTasks(tasks: readonly UploadTask[]) {
  let succeeded = 0
  let failed = 0
  let cancelled = 0
  let active = 0
  let totalBytes = 0
  let uploadedBytes = 0
  for (const task of tasks) {
    totalBytes += task.file.size
    if (isUploadTaskActive(task)) active++
    if (task.status === 'succeeded') {
      succeeded++
      uploadedBytes += task.file.size
    } else if (task.status === 'failed') failed++
    else if (task.status === 'cancelled') cancelled++
    if (task.status === 'uploading' || task.status === 'verifying') uploadedBytes += uploadLoadedBytes(task)
  }
  const allSucceeded = tasks.length > 0 && succeeded === tasks.length
  const percent = totalBytes
    ? Math.min(allSucceeded ? 100 : 99, Math.round(uploadedBytes / totalBytes * 100))
    : allSucceeded ? 100 : 0
  return { succeeded, failed, cancelled, active, totalBytes, uploadedBytes, percent }
}

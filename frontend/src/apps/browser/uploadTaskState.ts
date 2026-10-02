import type { UploadBatchItemState } from '../../shared/api/browser'
import type { OperationOutcome } from '../../shared/api/client'
import type { UploadTask } from './uploadQueue'

type StateText = (chinese: string, english: string) => string

// Interpret server evidence only. Requests, timers, announcements and directory
// refreshes belong to the queue, not to this state update.
export function applyUploadServerState(task: UploadTask, status: UploadBatchItemState, operation: OperationOutcome | undefined, text: StateText): void {
  if (status === 'cancelled' && operation?.commit === 'unknown') {
    task.status = 'cancelled'
    task.retryBlocked = true
    task.safeToPrepare = false
    task.cancelRequested = false
    task.pauseRequested = false
    task.error = text('系统正在自动确认上传结果，请稍后查看', 'The server is automatically checking the upload result; check again shortly')
    return
  }
  task.retryBlocked = false
  task.safeToPrepare = status === 'pending' || status === 'failed' || status === 'cancelled'
  if (status === 'complete') {
    task.loaded = task.file.size
    task.status = 'succeeded'
    task.error = operation?.cleanup !== 'complete'
      ? text('上传成功，临时数据仍在后台清理', 'Upload succeeded; temporary data is being cleaned up')
      : ''
    task.cancelRequested = false
    task.pauseRequested = false
    return
  }
  if (status === 'unknown') {
    markUploadUnconfirmed(task, text)
    return
  }
  if (status === 'in_progress') {
    task.status = 'verifying'
    task.retryBlocked = true
    task.error = text('操作仍在服务端执行，正在确认结果', 'The operation is still running on the server')
    return
  }
  if (status === 'cancelled' || (status === 'failed' && task.cancelRequested)) {
    task.status = 'cancelled'
    task.loaded = 0
    task.error = text('任务已终止，服务端确认未提交', 'Task terminated; the server confirmed it was not committed')
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
    ? text('服务端确认上传尚未开始，可以重试', 'The server confirmed the upload did not start; it can be retried')
    : text('服务端确认上传失败，可以重试', 'The server confirmed the upload failed; it can be retried')
  task.cancelRequested = false
  task.pauseRequested = false
}

export function markUploadUnconfirmed(task: UploadTask, text: StateText): void {
  task.status = 'verifying'
  task.retryBlocked = true
  task.error = text('系统正在自动确认上传结果或清理临时数据，请稍后查看', 'The server is checking the upload result or cleaning temporary data; check again shortly')
}

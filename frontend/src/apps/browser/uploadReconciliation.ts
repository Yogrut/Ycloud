import { getUploadBatchStatus } from '../../shared/api/browser'
import type { UploadBatchStatus } from '../../shared/api/browser'
import type { UploadTask } from './uploadQueue'
import { uploadTicketKey } from './uploadTaskGroups'

type BatchItem = UploadBatchStatus['items'][number]

interface ReconciliationContext {
  tasks: () => ReadonlySet<UploadTask>
  applyResult: (task: UploadTask, item: BatchItem) => void
  unconfirmed: (task: UploadTask) => void
}

interface BatchCheck {
  storageId: string
  ticket: string
  tasks: Set<UploadTask>
  retryIndex: number
  pending?: Promise<void>
  timer?: number
}

const RECONCILE_DELAYS_MS = [0, 1000, 3000, 7000, 15000, 30000] as const

export function indexUploadBatchItems(batch: UploadBatchStatus): Map<string, BatchItem> {
  const items = new Map<string, BatchItem>()
  for (const item of batch.items) {
    const path = item.path
    if (!items.has(path)) items.set(path, item)
  }
  return items
}

// One read/retry chain per storage-ticket pair, not one per file. This owns
// only result queries; cancellation and state interpretation stay in the queue.
export function createUploadReconciliation(context: ReconciliationContext) {
  const checks = new Map<string, BatchCheck>()
  let disposed = false

  function current(check: BatchCheck): boolean {
    return !disposed && checks.get(uploadTicketKey(check.storageId, check.ticket)) === check
  }

  function prune(check: BatchCheck): void {
    const retained = context.tasks()
    for (const task of check.tasks) {
      if (!retained.has(task) || task.status !== 'verifying'
        || task.storageId !== check.storageId || task.ticket !== check.ticket) check.tasks.delete(task)
    }
  }

  async function readBatch(check: BatchCheck): Promise<void> {
    try {
      if (!current(check)) return
      prune(check)
      if (!check.tasks.size) { checks.delete(uploadTicketKey(check.storageId, check.ticket)); return }
      let items: Map<string, BatchItem> | undefined
      try { items = indexUploadBatchItems(await getUploadBatchStatus(check.ticket, check.storageId)) }
      catch { /* A read failure supplies no commit evidence; use the same finite retry budget. */ }
      if (!current(check)) return
      prune(check)
      for (const task of check.tasks) {
        const item = items?.get(task.targetPath)
        if (!item) continue
        context.applyResult(task, item)
        if (item.status !== 'unknown' && item.status !== 'in_progress') check.tasks.delete(task)
      }
      const key = uploadTicketKey(check.storageId, check.ticket)
      if (!check.tasks.size) { checks.delete(key); return }
      const delay = RECONCILE_DELAYS_MS[check.retryIndex++]
      if (delay === undefined) {
        checks.delete(key)
        for (const task of check.tasks) context.unconfirmed(task)
        return
      }
      check.timer = window.setTimeout(() => {
        check.timer = undefined
        void start(check)
      }, delay)
    } finally {
      check.pending = undefined
    }
  }

  function start(check: BatchCheck): Promise<void> {
    if (check.pending) return check.pending
    // Gather synchronous requests before reading the shared response.
    check.pending = Promise.resolve().then(() => readBatch(check))
    return check.pending
  }

  function reconcile(task: UploadTask): Promise<void> {
    if (disposed || !task.ticket || task.status !== 'verifying') return Promise.resolve()
    const key = uploadTicketKey(task.storageId, task.ticket)
    let check = checks.get(key)
    if (!check) {
      check = { storageId: task.storageId, ticket: task.ticket, tasks: new Set(), retryIndex: 0 }
      checks.set(key, check)
    }
    check.tasks.add(task)
    if (check.timer !== undefined) {
      window.clearTimeout(check.timer)
      check.timer = undefined
    }
    return start(check)
  }

  function dispose(): void {
    disposed = true
    for (const check of checks.values()) {
      if (check.timer !== undefined) window.clearTimeout(check.timer)
    }
    checks.clear()
  }

  return { reconcile, dispose }
}

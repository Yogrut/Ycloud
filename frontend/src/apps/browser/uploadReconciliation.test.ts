import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { getUploadBatchStatus } from '../../shared/api/browser'
import type { UploadBatchStatus } from '../../shared/api/browser'
import type { UploadTask } from './uploadQueue'
import { createUploadReconciliation, indexUploadBatchItems } from './uploadReconciliation'

vi.mock('../../shared/api/browser', () => ({ getUploadBatchStatus: vi.fn() }))

function task(id: number, storageId = 'first', ticket = 'shared'): UploadTask {
  return { id, storageId, ticket, file: new File(['data'], `${id}.txt`), relativePath: `${id}.txt`,
    basePath: '', targetPath: `${id}.txt`, status: 'verifying', loaded: 0, error: '' }
}

function batch(tasks: UploadTask[], status: UploadBatchStatus['items'][number]['status']): UploadBatchStatus {
  return { ticket: 'shared', items: tasks.map(task => ({ path: task.targetPath, size: task.file.size, status })) }
}

function pending<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

const schedulers: Array<ReturnType<typeof createUploadReconciliation>> = []

function scheduler(tasks: UploadTask[]) {
  const retained = new Set(tasks)
  const context = { tasks: () => retained, applyResult: vi.fn(), unconfirmed: vi.fn() }
  const checks = createUploadReconciliation(context)
  schedulers.push(checks)
  return { checks, context, retained }
}

beforeEach(() => { vi.resetAllMocks(); vi.useFakeTimers() })
afterEach(() => { schedulers.splice(0).forEach(checks => checks.dispose()); vi.useRealTimers() })

describe('upload batch reconciliation', () => {
  it('shares the first read and its promise, including tasks added while it is in flight', async () => {
    const tasks = [task(1), task(2)]
    const { checks, context } = scheduler(tasks)
    const read = pending<UploadBatchStatus>()
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(read.promise)
    const first = checks.reconcile(tasks[0]!)
    await Promise.resolve()
    const second = checks.reconcile(tasks[1]!)
    expect(second).toBe(first)
    read.resolve(batch(tasks, 'complete'))
    await first
    expect(getUploadBatchStatus).toHaveBeenCalledExactlyOnceWith('shared', 'first')
    expect(context.applyResult.mock.calls.map(call => call[0])).toEqual(tasks)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not merge the same ticket across storages or different tickets within one storage', async () => {
    const tasks = [task(1), task(2, 'second'), task(3, 'first', 'other')]
    const { checks } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValue(batch(tasks, 'complete'))
    await Promise.all(tasks.map(checks.reconcile))
    expect(vi.mocked(getUploadBatchStatus).mock.calls).toEqual([['shared', 'first'], ['shared', 'second'], ['other', 'first']])
  })

  it('keeps only unresolved files in the shared retry chain', async () => {
    const tasks = [task(1), task(2)]
    const { checks, context } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'shared', items: [
      ...batch([tasks[0]!], 'complete').items, ...batch([tasks[1]!], 'unknown').items,
    ] }).mockResolvedValueOnce(batch(tasks, 'complete'))
    await Promise.all(tasks.map(checks.reconcile))
    expect(vi.getTimerCount()).toBe(1)
    await vi.runAllTimersAsync()
    expect(context.applyResult.mock.calls.map(call => call[0].id)).toEqual([1, 2, 2])
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(2)
    expect(vi.getTimerCount()).toBe(0)
  })

  it.each(['missing', 'network', 'unknown', 'in_progress'] as const)('ends %s checks after seven reads for the entire batch', async reason => {
    const tasks = [task(1), task(2)]
    const { checks, context } = scheduler(tasks)
    if (reason === 'network') vi.mocked(getUploadBatchStatus).mockRejectedValue(new Error('offline'))
    else vi.mocked(getUploadBatchStatus).mockResolvedValue(reason === 'missing' ? { ticket: 'shared', items: [] } : batch(tasks, reason))
    await Promise.all(tasks.map(checks.reconcile))
    await vi.runAllTimersAsync()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(7)
    expect(context.unconfirmed.mock.calls.map(call => call[0])).toEqual(tasks)
    expect(vi.getTimerCount()).toBe(0)
  })

  it.each(['failed', 'cancelled'] as const)('stops querying files with a settled %s result', async status => {
    const tasks = [task(1)]
    const { checks, context } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValue(batch(tasks, status))
    await checks.reconcile(tasks[0]!)
    await vi.runAllTimersAsync()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
    expect(context.applyResult).toHaveBeenCalledOnce()
    expect(context.unconfirmed).not.toHaveBeenCalled()
  })

  it('does not reset the retry budget when new files join a scheduled chain', async () => {
    const tasks = [task(1), task(2), task(3)]
    const { checks, context } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValue(batch(tasks, 'unknown'))
    for (const task of tasks) {
      await checks.reconcile(task)
      expect(vi.getTimerCount()).toBe(1)
    }
    await vi.runAllTimersAsync()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(7)
    expect(context.unconfirmed.mock.calls.map(call => call[0])).toEqual(tasks)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('ignores removed, rebound or already settled tasks when a read returns', async () => {
    const tasks = [task(1), task(2), task(3)]
    const { checks, context, retained } = scheduler(tasks)
    const read = pending<UploadBatchStatus>()
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(read.promise)
    const requests = tasks.map(checks.reconcile)
    await Promise.resolve()
    retained.delete(tasks[0]!)
    tasks[1]!.ticket = 'new-ticket'
    tasks[2]!.status = 'succeeded'
    read.resolve(batch(tasks, 'unknown'))
    await Promise.all(requests)
    expect(context.applyResult).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not issue a timed read after all its tasks are removed', async () => {
    const tasks = [task(1)]
    const { checks, retained } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValue(batch(tasks, 'unknown'))
    await checks.reconcile(tasks[0]!)
    retained.clear()
    await vi.runAllTimersAsync()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
  })

  it.each(['resolve', 'reject'] as const)('ignores an in-flight %s after disposal', async outcome => {
    const tasks = [task(1)]
    const { checks, context } = scheduler(tasks)
    const read = pending<UploadBatchStatus>()
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(read.promise)
    const request = checks.reconcile(tasks[0]!)
    await Promise.resolve()
    checks.dispose()
    if (outcome === 'resolve') read.resolve(batch(tasks, 'unknown'))
    else read.reject(new Error('offline'))
    await request
    expect(context.applyResult).not.toHaveBeenCalled()
    expect(context.unconfirmed).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
    await checks.reconcile(tasks[0]!)
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
  })

  it('clears a scheduled retry on disposal', async () => {
    const tasks = [task(1)]
    const { checks } = scheduler(tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValue(batch(tasks, 'in_progress'))
    await checks.reconcile(tasks[0]!)
    expect(vi.getTimerCount()).toBe(1)
    checks.dispose()
    await vi.runAllTimersAsync()
    expect(vi.getTimerCount()).toBe(0)
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
  })

  it('ignores tasks without a ticket or without a verifying state', async () => {
    const tasks = [{ ...task(1), ticket: undefined }, { ...task(2), status: 'succeeded' as const }]
    const { checks } = scheduler(tasks)
    await Promise.all(tasks.map(checks.reconcile))
    expect(getUploadBatchStatus).not.toHaveBeenCalled()
  })

  it('uses first-match indexing even if a response repeats a path', () => {
    const first = batch([task(1)], 'pending').items[0]!
    const duplicate = { ...first, status: 'complete' as const }
    expect(indexUploadBatchItems({ ticket: 'shared', items: [first, duplicate] }).get(first.path)).toBe(first)
  })
})

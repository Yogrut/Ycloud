import { ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cancelUploadBatch, getUploadBatchStatus, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import type { UploadBatchStatus } from '../../shared/api/browser'
import type { UploadTask } from './uploadQueue'
import { ApiError } from '../../shared/api/client'
import { useUploadQueue } from './useUploadQueue'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  prepareUploadBatch: vi.fn(), uploadFile: vi.fn(), getUploadBatchStatus: vi.fn(),
  cancelUploadBatch: vi.fn().mockResolvedValue(undefined),
}))

const queues: Array<ReturnType<typeof useUploadQueue>> = []

function uploads() {
  const context = {
    storageId: ref('first'), path: ref('original'), maxUploadBytes: ref(0),
    maxUploadBatchBytes: ref(0), maxUploadBatchEntries: ref(0),
    canUpload: vi.fn(() => true), requireUpload: vi.fn(() => true),
    announce: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined),
  }
  const queue = useUploadQueue(context)
  queues.push(queue)
  return { context, queue }
}

function delayedDrop() {
  let complete!: (file: File) => void
  let fail!: (error: DOMException) => void
  const entry = {
    isFile: true, isDirectory: false, name: 'file.txt', fullPath: '/file.txt',
    file: vi.fn((success: (file: File) => void, failure: (error: DOMException) => void) => {
      complete = success
      fail = failure
    }),
  } as unknown as FileSystemFileEntry
  const event = { dataTransfer: { items: [{ kind: 'file', webkitGetAsEntry: () => entry }], files: [] } } as unknown as DragEvent
  return { event, entry, complete: () => complete(new File(['data'], 'file.txt')), fail: () => fail(new DOMException('cannot read')) }
}

async function settle() {
  for (let index = 0; index < 16; index++) await Promise.resolve()
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(prepareUploadBatch).mockResolvedValue({ ticket: 'ticket' })
  vi.mocked(uploadFile).mockResolvedValue(undefined)
  vi.mocked(getUploadBatchStatus).mockResolvedValue({ ticket: 'ticket', items: [] })
})

function ticketedTask(id: number, storageId = 'first', status: UploadTask['status'] = 'paused'): UploadTask {
  return { id, storageId, ticket: 'shared', status, file: new File(['data'], `${id}.txt`),
    basePath: 'original', targetPath: `original/${id}.txt`, relativePath: `${id}.txt`, loaded: 0, error: '' }
}

describe('upload ticket grouping and mappings', () => {
  it('runs at most four files and replaces a finished worker without waiting for other files', async () => {
    const { queue } = uploads()
    const complete: Array<() => void> = []
    let active = 0
    let peak = 0
    vi.mocked(uploadFile).mockImplementation(() => {
      active++
      peak = Math.max(peak, active)
      return new Promise<void>(resolve => complete.push(() => { active--; resolve() }))
    })
    const files = Array.from({ length: 8 }, (_, index) => new File(['data'], `${index}.txt`))
    queue.uploadFiles({ target: { files, value: '' } } as unknown as Event)
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(4)
    complete[0]!()
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(5)
    for (let index = 1; index < 8; index++) { complete[index]!(); await settle() }
    expect(peak).toBe(4)
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
  })

  it('cancels only selected active files and frees their workers while results are being checked', async () => {
    const { queue } = uploads()
    const complete: Array<() => void> = []
    let checked!: (batch: UploadBatchStatus) => void
    vi.mocked(getUploadBatchStatus).mockImplementation(() => new Promise(resolve => { checked = resolve }))
    vi.mocked(uploadFile).mockImplementation((_path, _file, _progress, _storage, _ticket, signal) => new Promise<void>((resolve, reject) => {
      complete.push(resolve)
      signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true })
    }))
    const files = Array.from({ length: 6 }, (_, index) => new File(['data'], `${index}.txt`))
    queue.uploadFiles({ target: { files, value: '' } } as unknown as Event)
    await settle()
    const signals = vi.mocked(uploadFile).mock.calls.map(call => call[5]!)
    queue.pauseUploads([queue.uploadTasks.value[0]!.id])
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(5)
    queue.terminateUploads([queue.uploadTasks.value[1]!.id])
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(6)
    expect(signals.map(signal => signal.aborted)).toEqual([true, true, false, false])
    for (const finish of complete) finish()
    checked({ ticket: 'ticket', items: files.map((file, index) => ({ path: `original/${file.name}`, size: 4, status: index < 2 ? 'cancelled' : 'complete' })) })
    await settle()
    expect(queue.uploadTasks.value.slice(2).every(task => task.status === 'succeeded')).toBe(true)
  })

  it('ignores repeated and unknown ids and never cancels a verification-only selection', () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2, 'first', 'verifying'))
    queue.terminateUploads([2, 2, 99])
    expect(cancelUploadBatch).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value[1]?.status).toBe('verifying')
    queue.terminateUploads([1, 1, 99])
    expect(cancelUploadBatch).toHaveBeenCalledExactlyOnceWith('shared', 'first', ['original/1.txt'])
  })

  it('clears only terminal selections and preserves tickets required by active tasks', () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2, 'first', 'succeeded'),
      { ...ticketedTask(3, 'first', 'failed'), retryBlocked: true })
    queue.clearUploadTasks([1, 2, 3])
    expect(queue.uploadTasks.value).toHaveLength(3)
    expect(cancelUploadBatch).not.toHaveBeenCalled()
    queue.clearUploadTasks([2, 3, 3, 99])
    expect(queue.uploadTasks.value.map(task => task.id)).toEqual([1])
    expect(cancelUploadBatch).not.toHaveBeenCalled()
  })

  it('does not mutate records or issue task actions after disposal', async () => {
    const { context, queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2, 'first', 'failed'),
      ticketedTask(3, 'first', 'succeeded'), ticketedTask(4, 'first', 'queued'))
    queue.disposeUploads()
    vi.clearAllMocks()
    const records = queue.uploadTasks.value
    queue.pauseUploads([4])
    queue.resumeUploads([1])
    queue.terminateUploads([1, 4])
    queue.retryUpload(2)
    queue.removeFailedUpload(2)
    queue.clearUploadTasks([2, 3])
    await settle()
    expect(queue.uploadTasks.value).toBe(records)
    expect(records.map(task => task.status)).toEqual(['paused', 'failed', 'succeeded', 'queued'])
    expect(cancelUploadBatch).not.toHaveBeenCalled()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(getUploadBatchStatus).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
  })

  it('cancels only terminable paths in a mixed selection', () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2, 'first', 'succeeded'),
      ticketedTask(3, 'first', 'failed'), ticketedTask(4, 'first', 'cancelled'))
    queue.terminateUploads([1, 2, 3, 4])
    expect(cancelUploadBatch).toHaveBeenCalledExactlyOnceWith('shared', 'first', ['original/1.txt'])
    expect(queue.uploadTasks.value.map(task => task.status)).toEqual(['cancelled', 'succeeded', 'failed', 'cancelled'])
  })

  it('maps reordered, overlapping assigned names by original path and keeps the first duplicate', async () => {
    const { queue } = uploads()
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'numbered', upload_mode: 'direct', items: [
      { original_path: 'original/a (1).txt', path: 'original/a (2).txt' },
      { original_path: 'original/a.txt', path: 'original/a (1).txt' },
      { original_path: 'original/a.txt', path: 'original/wrong.txt' },
    ] })
    const files = ['a.txt', 'a (1).txt', 'unchanged.txt'].map(name => new File(['data'], name))
    await queue.handleUploadDrop({ dataTransfer: { items: [], files } } as unknown as DragEvent)
    expect(vi.mocked(uploadFile).mock.calls.map(call => [call[0], call[6]])).toEqual([
      ['original/a (1).txt', true], ['original/a (2).txt', true], ['original/unchanged.txt', true],
    ])
    expect(queue.uploadTasks.value.map(task => task.relativePath)).toEqual(['a (1).txt', 'a (2).txt', 'unchanged.txt'])
    expect(queue.uploadTasks.value.map(task => task.originalTargetPath)).toEqual(files.map(file => `original/${file.name}`))
  })

  it('reads each preparation mapping once instead of rescanning it for every task', async () => {
    const { queue } = uploads()
    const files = Array.from({ length: 64 }, (_, index) => new File([], `${index}.txt`))
    let originalPathReads = 0
    const items = files.map(file => ({
      get original_path() { originalPathReads++; return `original/${file.name}` },
      path: `original/assigned-${file.name}`,
    })).reverse()
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'numbered', items })
    await queue.handleUploadDrop({ dataTransfer: { items: [], files } } as unknown as DragEvent)
    expect(originalPathReads).toBe(files.length)
    expect(vi.mocked(uploadFile).mock.calls.map(call => call[0])).toEqual(files.map(file => `original/assigned-${file.name}`))
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
  })

  it('indexes a shared status response once and does not repeat completed uploads', async () => {
    const { queue } = uploads()
    const tasks = Array.from({ length: 64 }, (_, index) => ticketedTask(index))
    queue.uploadTasks.value.push(...tasks)
    let pathReads = 0
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'shared', items: tasks.map(task => ({
      get path() { pathReads++; return task.targetPath }, size: task.file.size, status: 'complete' as const,
    })).reverse() })
    queue.resumeUploads(tasks.map(task => task.id))
    await settle()
    expect(getUploadBatchStatus).toHaveBeenCalledExactlyOnceWith('shared', 'first')
    expect(pathReads).toBe(tasks.length)
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
  })

  it('checks the same ticket separately per storage and retains first-match status behavior', async () => {
    const { queue } = uploads()
    const first = ticketedTask(1)
    const second = { ...ticketedTask(2, 'second'), targetPath: first.targetPath }
    queue.uploadTasks.value.push(first, second)
    vi.mocked(getUploadBatchStatus).mockImplementation(async (ticket, storageId) => ({ ticket, items: [
      { path: first.targetPath, size: 4, status: storageId === 'first' ? 'complete' : 'pending' },
      { path: first.targetPath, size: 4, status: 'complete' },
    ] }))
    queue.resumeUploads([1, 2])
    await settle()
    expect(vi.mocked(getUploadBatchStatus).mock.calls).toEqual([['shared', 'first'], ['shared', 'second']])
    expect(uploadFile).toHaveBeenCalledExactlyOnceWith(second.targetPath, second.file, expect.any(Function), 'second', 'shared', expect.any(AbortSignal), undefined)
    expect(prepareUploadBatch).not.toHaveBeenCalled()
  })

  it('groups cancellation paths per storage and excludes tasks still being verified', () => {
    const { queue } = uploads()
    const tasks = [ticketedTask(1), ticketedTask(2), ticketedTask(3, 'second'), ticketedTask(4, 'first', 'verifying')]
    queue.uploadTasks.value.push(...tasks, { ...ticketedTask(5), ticket: undefined })
    queue.terminateUploads([1, 2, 3, 4, 5])
    expect(vi.mocked(cancelUploadBatch).mock.calls).toEqual([
      ['shared', 'first', ['original/1.txt', 'original/2.txt']], ['shared', 'second', ['original/3.txt']],
    ])
    expect(queue.uploadTasks.value[3]?.status).toBe('verifying')
    expect(queue.uploadTasks.value[4]?.status).toBe('cancelled')
  })

  it('retains pause and termination changes while awaiting numbered preparation results', async () => {
    const { queue } = uploads()
    let complete!: (value: Awaited<ReturnType<typeof prepareUploadBatch>>) => void
    vi.mocked(prepareUploadBatch).mockReturnValueOnce(new Promise(resolve => { complete = resolve }))
    const files = [new File(['data'], 'paused.txt'), new File(['data'], 'cancelled.txt')]
    queue.uploadFiles({ target: { files, value: '' } } as unknown as Event)
    queue.pauseUploads([1])
    queue.terminateUploads([2])
    complete({ ticket: 'numbered', items: [
      { original_path: 'original/cancelled.txt', path: 'original/cancelled (1).txt' },
      { original_path: 'original/paused.txt', path: 'original/paused (1).txt' },
    ] })
    await settle()
    expect(queue.uploadTasks.value[0]).toMatchObject({ status: 'paused', ticket: 'numbered', targetPath: 'original/paused (1).txt' })
    expect(queue.uploadTasks.value[1]).toMatchObject({ status: 'cancelled', targetPath: 'original/cancelled (1).txt' })
    expect(queue.uploadTasks.value[1]?.ticket).toBe('numbered')
    expect(cancelUploadBatch).toHaveBeenCalledExactlyOnceWith('numbered', 'first', ['original/cancelled (1).txt'])
    expect(uploadFile).not.toHaveBeenCalled()
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'numbered', items: [
      { path: 'original/paused (1).txt', size: 4, status: 'pending' },
    ] })
    queue.resumeUploads([1])
    await settle()
    expect(uploadFile).toHaveBeenCalledExactlyOnceWith('original/paused (1).txt', files[0], expect.any(Function), 'first', 'numbered', expect.any(AbortSignal), false)
    expect(prepareUploadBatch).toHaveBeenCalledTimes(1)
  })

  it('shares a failed status check within a ticket without preparing or uploading again', async () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2))
    vi.mocked(getUploadBatchStatus).mockRejectedValueOnce(new Error('offline'))
    queue.resumeUploads([1, 2])
    await settle()
    expect(getUploadBatchStatus).toHaveBeenCalledExactlyOnceWith('shared', 'first')
    expect(queue.uploadTasks.value.map(task => [task.status, task.error])).toEqual([['failed', 'offline'], ['failed', 'offline']])
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
  })

  it('retains a ticket needed by another failed task but releases the same ticket in another storage', () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1, 'first', 'failed'), ticketedTask(2, 'first', 'failed'),
      ticketedTask(3, 'second', 'succeeded'), ticketedTask(4, 'second', 'cancelled'))
    queue.clearUploadTasks([1, 3])
    expect(cancelUploadBatch).toHaveBeenCalledExactlyOnceWith('shared', 'second')
    queue.removeFailedUpload(2)
    expect(vi.mocked(cancelUploadBatch).mock.calls).toEqual([['shared', 'second'], ['shared', 'first']])
  })

  it('releases each storage-ticket pair once on disposal', () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2), ticketedTask(3, 'second'),
      { ...ticketedTask(4), ticket: undefined })
    queue.disposeUploads()
    expect(vi.mocked(cancelUploadBatch).mock.calls).toEqual([['shared', 'first', undefined, true], ['shared', 'second', undefined, true]])
  })
})

afterEach(() => {
  queues.splice(0).forEach(queue => queue.disposeUploads())
  vi.restoreAllMocks()
  vi.useRealTimers()
})

describe('upload advancement and result scheduling', () => {
  it('does not start another file after disposal while checking an old ticket', async () => {
    const { queue } = uploads()
    let now = 0
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    const completed: Array<() => void> = []
    vi.mocked(uploadFile).mockImplementation((_path, _file, _progress, _storage, _ticket, signal) => new Promise<void>((resolve, reject) => {
      completed.push(resolve)
      signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true })
    }))
    let checked!: (value: UploadBatchStatus) => void
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(new Promise(resolve => { checked = resolve }))
    queue.uploadFiles({ target: { files: Array.from({ length: 5 }, (_, index) => new File(['data'], `${index}.txt`)), value: '' } } as unknown as Event)
    await settle()
    now = 180_000
    completed[0]!()
    await settle()
    expect(getUploadBatchStatus).toHaveBeenCalledOnce()
    queue.disposeUploads()
    checked({ ticket: 'ticket', items: [{ path: 'original/4.txt', size: 4, status: 'pending' }] })
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(4)
    expect(prepareUploadBatch).toHaveBeenCalledOnce()
  })

  it('renews only never-started expired reservations when a long queue advances', async () => {
    const { queue } = uploads()
    let now = 0
    vi.spyOn(performance, 'now').mockImplementation(() => now)
    const completed: Array<() => void> = []
    vi.mocked(uploadFile).mockImplementation(() => new Promise<void>(resolve => completed.push(resolve)))
    queue.uploadFiles({ target: { files: Array.from({ length: 8 }, (_, index) => new File(['data'], `${index}.txt`)), value: '' } } as unknown as Event)
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(4)
    now = 180_000
    vi.mocked(getUploadBatchStatus).mockRejectedValueOnce(new ApiError('expired', 410, 'upload_batch_expired'))
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'renewed' })
    completed[0]!()
    await settle()
    expect(prepareUploadBatch).toHaveBeenCalledTimes(2)
    expect(vi.mocked(prepareUploadBatch).mock.calls[1]![0]).toHaveLength(4)
    expect(vi.mocked(uploadFile).mock.calls[4]![4]).toBe('renewed')
    expect(queue.uploadTasks.value[0]?.status).toBe('succeeded')
    for (let index = 1; index < 8; index++) { completed[index]!(); await settle() }
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
  })

  it('page exit cancels tickets with keepalive exactly once and aborts active uploads', async () => {
    const { queue } = uploads()
    vi.mocked(uploadFile).mockImplementation((_path, _file, _progress, _storage, _ticket, signal) => new Promise<void>((_resolve, reject) => {
      signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true })
    }))
    queue.uploadFiles({ target: { files: [new File(['data'], 'one.txt')], value: '' } } as unknown as Event)
    await settle()
    window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false }))
    await settle()
    queue.disposeUploads()
    expect(cancelUploadBatch).toHaveBeenCalledExactlyOnceWith('ticket', 'first', undefined, true)
  })

  it('retains a cancelled preparation ticket when its cancellation request fails', async () => {
    const { context, queue } = uploads()
    let prepared!: (value: { ticket: string }) => void
    vi.mocked(prepareUploadBatch).mockReturnValueOnce(new Promise(resolve => { prepared = resolve }))
    queue.uploadFiles({ target: { files: [new File(['data'], 'one.txt')], value: '' } } as unknown as Event)
    queue.terminateUploads([1])
    vi.mocked(cancelUploadBatch).mockRejectedValueOnce(new Error('cancel unavailable'))
    prepared({ ticket: 'cancelled-preparation' })
    await settle()
    expect(queue.uploadTasks.value[0]).toMatchObject({ status: 'cancelled', ticket: 'cancelled-preparation' })
    expect(context.announce).toHaveBeenCalledWith('cancel unavailable')
    queue.disposeUploads()
    expect(cancelUploadBatch).toHaveBeenLastCalledWith('cancelled-preparation', 'first', undefined, true)
  })
  it('coalesces completed-result refreshes into one read and one pending follow-up', async () => {
    const { context, queue } = uploads()
    let refreshed!: () => void
    context.refresh.mockReturnValueOnce(new Promise<void>(resolve => { refreshed = resolve }))
    const tasks = [ticketedTask(1), ticketedTask(2), ticketedTask(3)]
    queue.uploadTasks.value.push(...tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'shared', items: tasks.map(task => ({
      path: task.targetPath, size: task.file.size, status: 'complete',
    })) })
    queue.resumeUploads([1, 2, 3])
    await settle()
    expect(context.refresh).toHaveBeenCalledTimes(1)
    refreshed()
    await settle()
    expect(context.refresh).toHaveBeenCalledTimes(2)
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
  })

  it('ignores a refresh error and pending old-context refresh after navigation', async () => {
    const { context, queue } = uploads()
    let fail!: (error: Error) => void
    context.refresh.mockReturnValueOnce(new Promise<void>((_, reject) => { fail = reject }))
    const tasks = [ticketedTask(1), ticketedTask(2)]
    queue.uploadTasks.value.push(...tasks)
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'shared', items: tasks.map(task => ({
      path: task.targetPath, size: task.file.size, status: 'complete',
    })) })
    queue.resumeUploads([1, 2])
    await settle()
    context.path.value = 'other'
    fail(new Error('obsolete directory error'))
    await settle()
    expect(context.refresh).toHaveBeenCalledTimes(1)
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('ignores a refresh rejection after disposal and does not continue its pending refresh', async () => {
    const { context, queue } = uploads()
    let fail!: (error: Error) => void
    context.refresh.mockReturnValueOnce(new Promise<void>((_, reject) => { fail = reject }))
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'ticket', items: [
      { path: 'original/file.txt', size: 4, status: 'complete' },
    ] })
    await queue.handleUploadDrop({ dataTransfer: { items: [], files: [new File(['data'], 'file.txt')] } } as unknown as DragEvent)
    await settle()
    queue.disposeUploads()
    fail(new Error('obsolete refresh'))
    await settle()
    expect(context.refresh).toHaveBeenCalledTimes(1)
    expect(context.announce).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value[0]?.status).toBe('succeeded')
  })

  it('advances queued files without waiting for a slow directory refresh', async () => {
    const { context, queue } = uploads()
    let uploaded!: () => void
    let refreshed!: () => void
    vi.mocked(uploadFile).mockReturnValueOnce(new Promise<void>(resolve => { uploaded = resolve }))
    context.refresh.mockReturnValueOnce(new Promise<void>(resolve => { refreshed = resolve }))
    const pending = queue.handleUploadDrop({ dataTransfer: { items: [], files: [new File([], 'one.txt')] } } as unknown as DragEvent)
    await settle()
    queue.uploadFiles({ target: { files: [new File([], 'two.txt')], value: '' } } as unknown as Event)
    uploaded()
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(2)
    expect(context.refresh).toHaveBeenCalledTimes(1)
    refreshed()
    await pending
    await settle()
  })

  it('does not let refresh failure stop the queue or become an upload error', async () => {
    const { context, queue } = uploads()
    let uploaded!: () => void
    vi.mocked(uploadFile).mockReturnValueOnce(new Promise<void>(resolve => { uploaded = resolve }))
    context.refresh.mockRejectedValueOnce(new Error('directory unavailable'))
    const pending = queue.handleUploadDrop({ dataTransfer: { items: [], files: [new File([], 'one.txt')] } } as unknown as DragEvent)
    await settle()
    queue.uploadFiles({ target: { files: [new File([], 'two.txt')], value: '' } } as unknown as Event)
    uploaded()
    await pending
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(2)
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded' && !task.error)).toBe(true)
    expect(context.announce).toHaveBeenCalledWith('directory unavailable')
  })

  it('shares one in-flight batch query among tasks with the same storage and ticket', async () => {
    const { queue } = uploads()
    let checked!: (value: UploadBatchStatus) => void
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2))
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'shared', items: [
      { path: 'original/1.txt', size: 4, status: 'unknown' },
      { path: 'original/2.txt', size: 4, status: 'unknown' },
    ] }).mockReturnValue(new Promise(resolve => { checked = resolve }))
    queue.resumeUploads([1, 2])
    await settle()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(2)
    checked({ ticket: 'shared', items: [
      { path: 'original/1.txt', size: 4, status: 'complete' },
      { path: 'original/2.txt', size: 4, status: 'complete' },
    ] })
    await settle()
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
    expect(uploadFile).not.toHaveBeenCalled()
  })

  it('keeps a bounded batch retry chain instead of multiplying it per file', async () => {
    vi.useFakeTimers()
    const { queue } = uploads()
    queue.uploadTasks.value.push(ticketedTask(1), ticketedTask(2))
    vi.mocked(getUploadBatchStatus).mockResolvedValue({ ticket: 'shared', items: [
      { path: 'original/1.txt', size: 4, status: 'unknown' },
      { path: 'original/2.txt', size: 4, status: 'unknown' },
    ] })
    queue.resumeUploads([1, 2])
    await settle()
    await vi.runAllTimersAsync()
    // One initial ticket check, then seven checks for the shared batch.
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(8)
    expect(vi.getTimerCount()).toBe(0)
    expect(queue.uploadTasks.value.every(task => task.status === 'verifying' && task.retryBlocked)).toBe(true)
  })
})

describe('upload input lifecycle', () => {
  it('releases a prepared ticket if disposal occurs while cancelling a numbered task', async () => {
    const { queue } = uploads()
    let complete!: (value: Awaited<ReturnType<typeof prepareUploadBatch>>) => void
    let cancelled!: () => void
    vi.mocked(prepareUploadBatch).mockReturnValueOnce(new Promise(resolve => { complete = resolve }))
    vi.mocked(cancelUploadBatch).mockReturnValueOnce(new Promise<void>(resolve => { cancelled = resolve }))
    queue.uploadFiles({ target: { files: [new File([], 'file.txt')], value: '' } } as unknown as Event)
    queue.terminateUploads([1])
    complete({ ticket: 'late-ticket', items: [{ original_path: 'original/file.txt', path: 'original/file (1).txt' }] })
    await settle()
    queue.disposeUploads()
    cancelled()
    await settle()
    expect(vi.mocked(cancelUploadBatch).mock.calls).toEqual([
      ['late-ticket', 'first', ['original/file (1).txt']], ['late-ticket', 'first', undefined, true],
    ])
    expect(uploadFile).not.toHaveBeenCalled()
  })

  it('ignores a ticket query rejection after disposal without announcing or polling', async () => {
    const { context, queue } = uploads()
    let fail!: (error: Error) => void
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(new Promise((_, reject) => { fail = reject }))
    queue.uploadTasks.value.push({ ...ticketedTask(1), attempted: true })
    queue.resumeUploads([1])
    queue.disposeUploads()
    fail(new Error('late query error'))
    await settle()
    expect(context.announce).not.toHaveBeenCalled()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
  })

  it('does not refresh or prepare remaining tasks after a ticket query completes on a disposed queue', async () => {
    const { context, queue } = uploads()
    let complete!: (value: UploadBatchStatus) => void
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(new Promise(resolve => { complete = resolve }))
    queue.uploadTasks.value.push(ticketedTask(1), { ...ticketedTask(2), ticket: undefined })
    queue.resumeUploads([1, 2])
    queue.disposeUploads()
    complete({ ticket: 'shared', items: [{ path: 'original/1.txt', size: 4, status: 'complete' }] })
    await settle()
    expect(context.refresh).not.toHaveBeenCalled()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('aborts every active file and does not start queued files after disposal', async () => {
    const { context, queue } = uploads()
    let complete!: () => void
    const pending = new Promise<void>(resolve => { complete = resolve })
    vi.mocked(uploadFile).mockImplementation(() => pending)
    const files = Array.from({ length: 6 }, (_, index) => new File(['data'], `${index}.txt`))
    queue.uploadFiles({ target: { files, value: '' } } as unknown as Event)
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(4)
    const signals = vi.mocked(uploadFile).mock.calls.map(call => call[5]!)
    queue.disposeUploads()
    expect(signals.every(signal => signal.aborted)).toBe(true)
    complete()
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(4)
    expect(context.refresh).not.toHaveBeenCalled()
    expect(context.announce).not.toHaveBeenCalled()
  })

  it('ignores an upload rejection after disposal', async () => {
    const { context, queue } = uploads()
    let fail!: (error: Error) => void
    vi.mocked(uploadFile).mockReturnValueOnce(new Promise<void>((_, reject) => { fail = reject }))
    queue.uploadFiles({ target: { files: [new File([], 'file.txt')], value: '' } } as unknown as Event)
    await settle()
    queue.disposeUploads()
    fail(new Error('late upload error'))
    await settle()
    expect(context.announce).not.toHaveBeenCalled()
    expect(getUploadBatchStatus).not.toHaveBeenCalled()
  })

  it('ignores a preparation rejection after disposal', async () => {
    const { context, queue } = uploads()
    let fail!: (error: Error) => void
    vi.mocked(prepareUploadBatch).mockReturnValueOnce(new Promise((_, reject) => { fail = reject }))
    queue.uploadFiles({ target: { files: [new File([], 'file.txt')], value: '' } } as unknown as Event)
    queue.disposeUploads()
    fail(new Error('late preparation error'))
    await settle()
    expect(context.announce).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
  })

  it('binds a delayed drop to its original storage and directory', async () => {
    const { context, queue } = uploads()
    const drop = delayedDrop()
    const pending = queue.handleUploadDrop(drop.event)
    context.storageId.value = 'second'
    context.path.value = 'changed'
    drop.complete()
    await pending
    expect(prepareUploadBatch).toHaveBeenCalledWith([{ path: 'original/file.txt', size: 4 }], 'first')
    expect(queue.uploadTasks.value[0]).toMatchObject({ storageId: 'first', basePath: 'original', status: 'succeeded' })
    expect(context.refresh).not.toHaveBeenCalled()
  })

  it('does not report an obsolete read error after disposal', async () => {
    const { context, queue } = uploads()
    const drop = delayedDrop()
    const pending = queue.handleUploadDrop(drop.event)
    queue.disposeUploads()
    drop.fail()
    await pending
    expect(context.announce).not.toHaveBeenCalled()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
  })

  it('settles a cancelled drop without waiting for the filesystem callback', async () => {
    const { context, queue } = uploads()
    const drop = delayedDrop()
    let settled = false
    const pending = queue.handleUploadDrop(drop.event).then(() => { settled = true })
    queue.disposeUploads()
    await settle()
    expect(settled).toBe(true)
    drop.complete()
    await pending
    expect(context.announce).not.toHaveBeenCalled()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value).toHaveLength(0)
  })

  it('does not start collecting drops after disposal or without upload permission', async () => {
    const { context, queue } = uploads()
    const drop = delayedDrop()
    context.requireUpload.mockReturnValue(false)
    await queue.handleUploadDrop(drop.event)
    expect(drop.entry.file).not.toHaveBeenCalled()
    context.requireUpload.mockReturnValue(true)
    queue.disposeUploads()
    await queue.handleUploadDrop(drop.event)
    expect(drop.entry.file).not.toHaveBeenCalled()
  })

  it('reports a current read failure without preparing a partial batch', async () => {
    const { context, queue } = uploads()
    const drop = delayedDrop()
    const pending = queue.handleUploadDrop(drop.event)
    drop.fail()
    await pending
    expect(context.announce).toHaveBeenCalledWith('cannot read')
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value).toHaveLength(0)
  })

  it('clears the file input and reports an oversized selection without throwing', async () => {
    const { context, queue } = uploads()
    const input = { files: Array.from({ length: 20_001 }, () => new File([], 'file.txt')), value: 'selected' }
    expect(() => queue.uploadFiles({ target: input } as unknown as Event)).not.toThrow()
    await settle()
    expect(input.value).toBe('')
    expect(context.announce).toHaveBeenCalledWith(expect.stringContaining('20000'))
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value).toHaveLength(0)
  })

  it('clears the selected input without reading when permission is denied', () => {
    const { context, queue } = uploads()
    context.requireUpload.mockReturnValue(false)
    const input = { files: [new File([], 'file.txt')], value: 'selected' }
    queue.uploadFiles({ target: input } as unknown as Event)
    expect(input.value).toBe('')
    expect(prepareUploadBatch).not.toHaveBeenCalled()
  })

  it('releases a ticket prepared while the queue is disposed without uploading', async () => {
    const { queue } = uploads()
    let complete!: (value: { ticket: string }) => void
    vi.mocked(prepareUploadBatch).mockReturnValueOnce(new Promise(resolve => { complete = resolve }))
    queue.uploadFiles({ target: { files: [new File([], 'file.txt')], value: '' } } as unknown as Event)
    queue.disposeUploads()
    complete({ ticket: 'late-ticket' })
    await settle()
    expect(cancelUploadBatch).toHaveBeenCalledWith('late-ticket', 'first')
    expect(uploadFile).not.toHaveBeenCalled()
  })
})

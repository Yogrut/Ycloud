import { ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createFolder, getUploadBatchStatus, listFiles, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import type { OperationOutcome } from '../../shared/api/client'
import type { UploadTask } from './uploadQueue'
import { MAX_UPLOAD_QUEUE_TASKS } from './uploadCandidates'
import { useUploadQueue } from './useUploadQueue'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  prepareUploadBatch: vi.fn(), uploadFile: vi.fn(), getUploadBatchStatus: vi.fn(),
  createFolder: vi.fn(), listFiles: vi.fn(),
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

async function settle() {
  for (let index = 0; index < 16; index++) await Promise.resolve()
}

function drop(...files: File[]): DragEvent {
  return { dataTransfer: { items: [], files } } as unknown as DragEvent
}

function outcome(commit: string): OperationOutcome {
  return { commit, cleanup: 'pending', retry: 'do_not_repeat' } as unknown as OperationOutcome
}

function filler(count: number): UploadTask[] {
  const file = new File([], 'filler.txt')
  return Array.from({ length: count }, (_, index) => ({
    id: index + 1, file, storageId: 'first', basePath: 'original',
    relativePath: `filler-${index}.txt`, targetPath: `original/filler-${index}.txt`,
    loaded: 0, error: '', status: 'failed',
  })) as unknown as UploadTask[]
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(prepareUploadBatch).mockResolvedValue({ ticket: 'ticket' })
  vi.mocked(uploadFile).mockResolvedValue(undefined)
  vi.mocked(getUploadBatchStatus).mockResolvedValue({ ticket: 'ticket', items: [] })
  vi.mocked(createFolder).mockResolvedValue({ success: true })
  vi.mocked(listFiles).mockRejectedValue(new ApiError('Not Found', 404, 'not_found'))
})

afterEach(() => {
  for (const queue of queues.splice(0)) queue.disposeUploads()
})

describe('upload failures and completion context', () => {
  it('marks a plain upload error as failed and announces it once', async () => {
    const { context, queue } = uploads()
    vi.mocked(uploadFile).mockRejectedValueOnce(new Error('boom'))
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    const task = queue.uploadTasks.value[0]
    expect(task?.status).toBe('failed')
    expect(task?.safeToPrepare).toBe(false)
    expect(context.announce).toHaveBeenCalledTimes(1)
    expect(context.announce).toHaveBeenCalledWith('boom')
  })

  it('moves to verification when the error blocks retry', async () => {
    const { context, queue } = uploads()
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockReturnValue(new Promise(() => {}))
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    const task = queue.uploadTasks.value[0]
    expect(task?.status).toBe('verifying')
    expect(task?.retryBlocked).toBe(true)
    expect(task?.safeToPrepare).toBe(false)
    expect(context.announce).not.toHaveBeenCalled()
    expect(getUploadBatchStatus).toHaveBeenCalled()
  })

  it('treats an unknown commit as verification instead of a retryable failure', async () => {
    const { queue } = uploads()
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('later', 503, undefined, undefined, outcome('unknown')))
    vi.mocked(getUploadBatchStatus).mockReturnValue(new Promise(() => {}))
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    const task = queue.uploadTasks.value[0]
    expect(task?.status).toBe('verifying')
    expect(task?.safeToPrepare).toBe(false)
    expect(task?.retryBlocked).toBe(true)
  })

  it('allows a rebuild after the batch expired', async () => {
    const { context, queue } = uploads()
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('expired', 410, 'upload_batch_expired'))
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    const task = queue.uploadTasks.value[0]
    expect(task?.status).toBe('failed')
    expect(task?.safeToPrepare).toBe(true)
    expect(context.announce).toHaveBeenCalledTimes(1)
    expect(context.announce).toHaveBeenCalledWith('expired')
  })

  it('allows a rebuild after a confirmed not-committed result', async () => {
    const { queue } = uploads()
    vi.mocked(uploadFile).mockRejectedValueOnce(
      new ApiError('not committed', 409, undefined, undefined, outcome('not_committed')),
    )
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    const task = queue.uploadTasks.value[0]
    expect(task?.status).toBe('failed')
    expect(task?.safeToPrepare).toBe(true)
  })

  it('refreshes once when the upload finishes in the directory it started in', async () => {
    const { context, queue } = uploads()
    await queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    expect(context.refresh).toHaveBeenCalledTimes(1)
  })

  it('does not refresh when the user navigated away before the upload finished', async () => {
    const { context, queue } = uploads()
    let finish!: () => void
    vi.mocked(uploadFile).mockReturnValueOnce(new Promise<void>(resolve => { finish = resolve }))
    const pending = queue.handleUploadDrop(drop(new File(['data'], 'file.txt')))
    await settle()
    context.path.value = 'elsewhere'
    finish()
    await pending
    await settle()
    expect(context.refresh).not.toHaveBeenCalled()
  })
})

describe('upload queue capacity', () => {
  it('accepts the last free slot', async () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push(...filler(MAX_UPLOAD_QUEUE_TASKS - 1))
    queue.uploadFiles({ target: { files: [new File(['data'], 'last.txt')], value: '' } } as unknown as Event)
    await settle()
    expect(queue.uploadTasks.value.length).toBe(MAX_UPLOAD_QUEUE_TASKS)
    expect(prepareUploadBatch).toHaveBeenCalled()
  })

  it('rejects one file beyond the limit without preparing', async () => {
    const { context, queue } = uploads()
    queue.uploadTasks.value.push(...filler(MAX_UPLOAD_QUEUE_TASKS))
    queue.uploadFiles({ target: { files: [new File(['data'], 'overflow.txt')], value: '' } } as unknown as Event)
    await settle()
    expect(queue.uploadTasks.value.length).toBe(MAX_UPLOAD_QUEUE_TASKS)
    expect(context.announce).toHaveBeenCalledTimes(1)
    expect(prepareUploadBatch).not.toHaveBeenCalled()
  })
})

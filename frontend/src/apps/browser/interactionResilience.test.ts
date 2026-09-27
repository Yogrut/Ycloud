import { ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BrowserCapabilities, FileEntry, FileListResponse, UploadBatchStatus } from '../../shared/api/browser'
import { batchOperation, checkDownload, getUploadBatchStatus, listFiles, listStorages, prepareArchive, prepareUploadBatch, uploadFile } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useBrowserFileOperations } from './useBrowserFileOperations'
import { useBrowserListing } from './useBrowserListing'
import { useUploadQueue } from './useUploadQueue'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  batchOperation: vi.fn(), listFiles: vi.fn(), listStorages: vi.fn(),
  checkDownload: vi.fn(), prepareArchive: vi.fn(),
  prepareUploadBatch: vi.fn(), uploadFile: vi.fn(), getUploadBatchStatus: vi.fn(),
  cancelUploadBatch: vi.fn().mockResolvedValue(undefined),
}))

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

async function settle() {
  for (let index = 0; index < 12; index++) await Promise.resolve()
}

const capabilities: BrowserCapabilities = { download: true, upload: true, create_directory: true, rename: true, move_items: true, copy: true, delete: true }
const completed = { success: 1, failed: 0, results: [] }

function operations() {
  const context = {
    path: ref(''), storageId: ref('primary'), entries: ref<FileEntry[]>([]),
    capabilities: ref(capabilities), maxArchiveBytes: ref(0), maxArchiveEntries: ref(0),
    selected: ref(new Set<string>()), requireCapability: () => true,
    announce: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined),
  }
  return { context, actions: useBrowserFileOperations(context) }
}

function listingData(path: string): FileListResponse {
  return { storage_id: 'primary', storages: [{ id: 'primary', name: 'Primary', requires_login: false }],
    current_path: path, parent_path: null, entries: [], page_start: 0, page_size: 20,
    next_cursor: null, can_write: true, max_upload_bytes: 0, max_archive_bytes: 0, max_archive_entries: 0 }
}

function listing() {
  const context = { announce: vi.fn(), resetSelection: vi.fn(), openLockedEntry: vi.fn(), requestStorageLogin: vi.fn() }
  return { context, view: useBrowserListing(context) }
}

function uploads() {
  const context = { storageId: ref('primary'), path: ref(''), maxUploadBytes: ref(0),
    maxUploadBatchBytes: ref(0), maxUploadBatchEntries: ref(0), canUpload: () => true,
    requireUpload: () => true, announce: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined) }
  const queue = useUploadQueue(context)
  const add = (files: File[]) => queue.uploadFiles({ target: { files, value: '' } } as unknown as Event)
  return { context, queue, add }
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(prepareUploadBatch).mockResolvedValue({ ticket: 'ticket' })
})
afterEach(() => vi.useRealTimers())

describe('file operation resilience', () => {
  it('does not stack identical download checks or archive preparations on repeated clicks', async () => {
    const download = deferred<void>()
    const archive = deferred<Awaited<ReturnType<typeof prepareArchive>>>()
    vi.mocked(checkDownload).mockReturnValue(download.promise)
    vi.mocked(prepareArchive).mockReturnValue(archive.promise)
    const { actions } = operations()
    const pendingDownload = actions.startDownload('one.txt')
    const pendingArchive = actions.startArchive(['one.txt', 'two.txt'])
    await Promise.all(Array.from({ length: 20 }, async () => {
      await actions.startDownload('one.txt')
      await actions.startArchive(['one.txt', 'two.txt'])
    }))
    expect(checkDownload).toHaveBeenCalledTimes(1)
    expect(prepareArchive).toHaveBeenCalledTimes(1)
    download.reject(new TypeError('offline'))
    archive.reject(new TypeError('offline'))
    await Promise.all([pendingDownload, pendingArchive])
    vi.mocked(checkDownload).mockRejectedValue(new TypeError('offline'))
    vi.mocked(prepareArchive).mockRejectedValue(new TypeError('offline'))
    await actions.startDownload('one.txt')
    await actions.startArchive(['one.txt', 'two.txt'])
    expect(checkDownload).toHaveBeenCalledTimes(2)
    expect(prepareArchive).toHaveBeenCalledTimes(2)
  })
  it('submits a burst of delete confirmations once, retaining the source storage', async () => {
    const pending = deferred<typeof completed>()
    vi.mocked(batchOperation).mockReturnValue(pending.promise)
    const { context, actions } = operations()
    actions.requestDelete(['one.txt'])
    context.storageId.value = 'other-storage'
    const first = actions.confirmDelete()
    await Promise.all(Array.from({ length: 20 }, () => actions.confirmDelete()))
    actions.requestDelete(['other.txt'])
    actions.requestTransfer('copy', ['other.txt'])
    expect(batchOperation).toHaveBeenCalledTimes(1)
    expect(batchOperation).toHaveBeenCalledWith('delete', ['one.txt'], '', 'primary')
    expect(actions.pendingDelete.value).toEqual(['one.txt'])
    expect(actions.pickerOperation.value).toBeNull()
    pending.resolve(completed)
    await first
    await actions.confirmDelete()
    expect(batchOperation).toHaveBeenCalledTimes(1)
    expect(actions.operationBusy.value).toBe(false)
  })

  it('releases busy state after a confirmed error, allowing a corrected retry', async () => {
    vi.mocked(batchOperation).mockRejectedValueOnce(new ApiError('Denied', 403)).mockResolvedValueOnce(completed)
    const { actions } = operations()
    actions.requestDelete(['one.txt'])
    await actions.confirmDelete()
    expect(actions.operationBusy.value).toBe(false)
    expect(actions.showDelete.value).toBe(true)
    await actions.confirmDelete()
    expect(batchOperation).toHaveBeenCalledTimes(2)
    expect(actions.showDelete.value).toBe(false)
  })

  it('does not repeat a delete when the connection was lost after submission', async () => {
    vi.mocked(batchOperation).mockRejectedValue(new ApiError('Verify first', 0, 'operation_result_unknown'))
    const { actions } = operations()
    actions.requestDelete(['one.txt'])
    await actions.confirmDelete()
    await actions.confirmDelete()
    expect(batchOperation).toHaveBeenCalledTimes(1)
    expect(actions.operationBusy.value).toBe(false)
  })
})

describe('upload reservation recovery', () => {
  it('does not queue the original file again after the server assigns a numbered name', async () => {
    const pending = deferred<void>()
    vi.mocked(uploadFile).mockReturnValueOnce(pending.promise)
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'numbered', items: [{ original_path: 'one.txt', path: 'one (2).txt' }] })
    const { add, queue } = uploads()
    const file = new File(['one'], 'one.txt')
    const first = add([file])
    await settle()
    expect(queue.uploadTasks.value[0]?.targetPath).toBe('one (2).txt')
    await add([file])
    expect(queue.uploadTasks.value).toHaveLength(1)
    expect(prepareUploadBatch).toHaveBeenCalledTimes(1)
    pending.resolve()
    await first
    queue.disposeUploads()
  })

  it('prepares a fresh ticket for an expired file that never started', async () => {
    const { queue } = uploads()
    const file = new File(['one'], 'one.txt')
    queue.uploadTasks.value.push({ id: 1, file, relativePath: 'one (2).txt', basePath: '', storageId: 'primary', targetPath: 'one (2).txt', originalTargetPath: 'one.txt', ticket: 'expired', status: 'paused', loaded: 0, error: '' })
    vi.mocked(getUploadBatchStatus).mockRejectedValueOnce(new ApiError('Expired', 410, 'upload_batch_expired'))
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'fresh', items: [{ original_path: 'one.txt', path: 'one (3).txt' }] })
    vi.mocked(uploadFile).mockResolvedValueOnce(undefined)
    queue.resumeUploads([1])
    await settle()
    expect(prepareUploadBatch).toHaveBeenCalledWith([{ path: 'one.txt', size: 3 }], 'primary')
    expect(queue.uploadTasks.value[0]?.status).toBe('succeeded')
    expect(uploadFile).toHaveBeenCalledWith('one (3).txt', file, expect.any(Function), 'primary', 'fresh', expect.any(AbortSignal), false)
    queue.disposeUploads()
  })

  it('never obtains a new ticket when an attempted upload result is missing', async () => {
    const { queue } = uploads()
    queue.uploadTasks.value.push({ id: 1, file: new File(['one'], 'one.txt'), relativePath: 'one.txt', basePath: '', storageId: 'primary', targetPath: 'one.txt', originalTargetPath: 'one.txt', ticket: 'expired', attempted: true, status: 'paused', loaded: 0, error: '' })
    vi.mocked(getUploadBatchStatus).mockRejectedValueOnce(new ApiError('Expired', 410, 'upload_batch_expired'))
    queue.resumeUploads([1])
    await settle()
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(uploadFile).not.toHaveBeenCalled()
    expect(queue.uploadTasks.value[0]?.retryBlocked).toBe(true)
    queue.disposeUploads()
  })
})

describe('listing resilience', () => {
  it('cancels superseded requests during rapid navigation and ignores late responses', async () => {
    const first = deferred<FileListResponse>()
    vi.mocked(listFiles).mockReturnValueOnce(first.promise).mockResolvedValueOnce(listingData('latest'))
    const { view } = listing()
    const old = view.navigate('old')
    const signal = vi.mocked(listFiles).mock.calls[0]?.[2]?.signal
    await view.navigate('latest')
    expect(signal?.aborted).toBe(true)
    first.resolve(listingData('old'))
    await old
    expect(view.path.value).toBe('latest')
    expect(view.loading.value).toBe(false)
    view.disposeListing()
  })

  it('does not apply a stale storage fallback or its error to a newer listing', async () => {
    const fallback = deferred<Awaited<ReturnType<typeof listStorages>>>()
    vi.mocked(listStorages).mockReturnValueOnce(fallback.promise)
    vi.mocked(listFiles).mockRejectedValueOnce(new Error('offline')).mockResolvedValueOnce(listingData('latest'))
    const { view, context } = listing()
    const old = view.refresh()
    await settle()
    expect(listStorages).toHaveBeenCalledTimes(1)
    await view.navigate('latest')
    fallback.resolve([{ id: 'obsolete', name: 'Obsolete', requires_login: false }])
    await old
    expect(view.storages.value[0]?.id).toBe('primary')
    expect(context.announce).not.toHaveBeenCalled()
    view.disposeListing()
  })

  it('aborts the active request on disposal and refuses its late result', async () => {
    const pending = deferred<FileListResponse>()
    vi.mocked(listFiles).mockReturnValueOnce(pending.promise)
    const { view } = listing()
    const request = view.refresh()
    const signal = vi.mocked(listFiles).mock.calls[0]?.[2]?.signal
    view.disposeListing()
    pending.resolve(listingData('late'))
    await request
    expect(signal?.aborted).toBe(true)
    expect(view.path.value).toBe('')
  })
})

describe('upload queue resilience', () => {
  it('uses server-assigned duplicate names for upload and visible task records', async () => {
    vi.mocked(prepareUploadBatch).mockResolvedValueOnce({ ticket: 'numbered', items: [{ original_path: 'one.txt', path: 'one (6).txt' }] })
    vi.mocked(uploadFile).mockResolvedValue(undefined)
    const { add, queue } = uploads()
    await add([new File(['one'], 'one.txt')])
    await settle()
    expect(vi.mocked(uploadFile).mock.calls[0]?.[0]).toBe('one (6).txt')
    expect(queue.uploadTasks.value[0]?.targetPath).toBe('one (6).txt')
    expect(queue.uploadTasks.value[0]?.relativePath).toBe('one (6).txt')
    expect(queue.uploadTasks.value[0]?.status).toBe('succeeded')
    queue.disposeUploads()
  })
  it('ignores repeated drops of an active target while keeping uploads sequential', async () => {
    const pending = deferred<void>()
    vi.mocked(uploadFile).mockReturnValueOnce(pending.promise).mockResolvedValue(undefined)
    const { add, queue, context } = uploads()
    const first = new File(['one'], 'one.txt')
    add([first])
    await settle()
    add([first, new File(['two'], 'two.txt')])
    await settle()
    expect(queue.uploadTasks.value).toHaveLength(2)
    expect(uploadFile).toHaveBeenCalledTimes(1)
    expect(context.announce).toHaveBeenCalled()
    pending.resolve()
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(2)
    expect(queue.uploadTasks.value.every(task => task.status === 'succeeded')).toBe(true)
    queue.disposeUploads()
  })

  it('bounds retained queue admission without starting an oversized backlog', async () => {
    const { add, queue, context } = uploads()
    const files = Array.from({ length: 20_001 }, (_, index) => new File([], `${index}.txt`))
    add(files)
    await settle()
    expect(queue.uploadTasks.value).toHaveLength(0)
    expect(prepareUploadBatch).not.toHaveBeenCalled()
    expect(context.announce).toHaveBeenCalled()
    queue.disposeUploads()
  })

  it('reconciles a lost upload response without uploading the file again', async () => {
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'ticket', items: [{ path: 'one.txt', size: 3, status: 'complete' }] })
    const { add, queue } = uploads()
    add([new File(['one'], 'one.txt')])
    await settle()
    expect(queue.uploadTasks.value[0]?.status).toBe('succeeded')
    expect(uploadFile).toHaveBeenCalledTimes(1)
    queue.disposeUploads()
  })

  it('does not treat an administrator-closed uncertain task as an uncommitted upload', async () => {
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockResolvedValueOnce({ ticket: 'ticket', items: [{
      path: 'one.txt', size: 3, status: 'cancelled',
      operation: { commit: 'unknown', cleanup: 'unknown', retry: 'verify_first' },
    }] })
    const { add, queue } = uploads()
    add([new File(['one'], 'one.txt')])
    await settle()
    const task = queue.uploadTasks.value[0]!
    expect(task.status).toBe('cancelled')
    expect(task.retryBlocked).toBe(true)
    expect(task.safeToPrepare).toBe(false)
    expect(task.error).not.toContain('确认未提交')
    queue.retryUpload(task.id)
    await settle()
    expect(uploadFile).toHaveBeenCalledTimes(1)
    queue.disposeUploads()
  })

  it('stops reconciliation at its retry budget when the network remains unavailable', async () => {
    vi.useFakeTimers()
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockRejectedValue(new TypeError('offline'))
    const { add, queue } = uploads()
    add([new File(['one'], 'one.txt')])
    await settle()
    await vi.runAllTimersAsync()
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(7)
    expect(queue.uploadTasks.value[0]?.status).toBe('failed')
    expect(queue.uploadTasks.value[0]?.retryBlocked).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
    queue.disposeUploads()
  })

  it('does not create new retry timers when a status response arrives after disposal', async () => {
    vi.useFakeTimers()
    const pending = deferred<UploadBatchStatus>()
    vi.mocked(uploadFile).mockRejectedValueOnce(new ApiError('offline', 0, 'operation_result_unknown'))
    vi.mocked(getUploadBatchStatus).mockReturnValueOnce(pending.promise)
    const { add, queue } = uploads()
    add([new File(['one'], 'one.txt')])
    await settle()
    queue.disposeUploads()
    pending.resolve({ ticket: 'ticket', items: [{ path: 'one.txt', size: 3, status: 'in_progress' }] })
    await settle()
    expect(vi.getTimerCount()).toBe(0)
    expect(getUploadBatchStatus).toHaveBeenCalledTimes(1)
  })
})

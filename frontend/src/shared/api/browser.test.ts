import { afterEach, describe, expect, it, vi } from 'vitest'
import { batchOperation, cancelUploadBatch, checkDownload, createFolder, downloadUrl, fileApi, getUploadBatchStatus, isPreviewTrafficExhausted, prepareArchive, prepareUploadBatch, uploadFile } from './browser'
import { formatSize } from '../format'

describe('browser API paths', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('checks download admission without consuming file content', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 429 }))
    vi.stubGlobal('fetch', fetchMock)
    await expect(checkDownload('/api/download?path=test')).rejects.toMatchObject({ status: 429 })
    expect(fetchMock).toHaveBeenCalledWith('/api/download?path=test', expect.objectContaining({ method: 'HEAD' }))
  })

  it('distinguishes a preview traffic denial with a body-free HEAD request', async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(null, { status: 429 })).mockResolvedValueOnce(new Response(null, { status: 200 }))
    vi.stubGlobal('fetch', fetchMock)
    expect(await isPreviewTrafficExhausted('/api/preview?path=audio.flac')).toBe(true)
    expect(await isPreviewTrafficExhausted('/api/preview?path=video.mp4')).toBe(false)
    expect(fetchMock).toHaveBeenCalledWith('/api/preview?path=audio.flac', expect.objectContaining({ method: 'HEAD', credentials: 'same-origin' }))
  })

  it('keeps the root endpoint minimal', () => {
    expect(fileApi('')).toBe('/api/files')
    expect(fileApi('/')).toBe('/api/files')
  })

  it('encodes complete nested paths as one query value', () => {
    expect(fileApi('/中文/space name/')).toBe('/api/files?path=%2F%E4%B8%AD%E6%96%87%2Fspace+name')
  })

  it('encodes directory pagination, search, and sort as one listing request', () => {
    expect(fileApi('', 'primary', {
      limit: 20,
      cursor: 'MjA',
      search: '测试 文件',
      sort: 'time',
      direction: 'desc',
    })).toBe('/api/files?storage_id=primary&limit=20&cursor=MjA&search=%E6%B5%8B%E8%AF%95+%E6%96%87%E4%BB%B6&sort=time&direction=desc')
  })

  it('marks gallery listings without changing the fixed page size', () => {
    expect(fileApi('photos', 'primary', {
      limit: 20,
      sort: 'name',
      direction: 'asc',
      gallery: true,
    })).toBe('/api/files?path=%2Fphotos&storage_id=primary&limit=20&sort=name&direction=asc&gallery=true')
  })

  it('creates a folder below the current path without client-side path concatenation', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await createFolder('/中文/space name/', '新的目录')

    expect(fetchMock).toHaveBeenCalledWith('/api/mkdir?path=%2F%E4%B8%AD%E6%96%87%2Fspace%20name', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ name: '新的目录' }),
    }))
  })

  it('encodes a UTF-8 download path without changing the file name', () => {
    expect(downloadUrl('/音乐/旅人 王铮亮.flac')).toBe('/api/download?path=%2F%E9%9F%B3%E4%B9%90%2F%E6%97%85%E4%BA%BA%20%E7%8E%8B%E9%93%AE%E4%BA%AE.flac')
  })

  it('prepares archives with normalized absolute request paths', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      ticket: 'ticket', total_bytes: 12, file_count: 1, entry_count: 1, max_bytes: 1024, max_entries: 10,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)

    await prepareArchive(['one.txt', '/folder/two.txt'])

    expect(fetchMock).toHaveBeenCalledWith('/api/archive/prepare', expect.objectContaining({
      body: JSON.stringify({ paths: ['/one.txt', '/folder/two.txt'] }),
    }))
  })

  it('prepares an upload batch without rewriting its relative target paths', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ ticket: 'batch-ticket' }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await expect(prepareUploadBatch([
      { path: 'folder/one.txt', size: 11 },
      { path: 'folder/two.txt', size: 22 },
    ], 'primary')).resolves.toEqual({ ticket: 'batch-ticket' })

    expect(fetchMock).toHaveBeenCalledWith('/api/upload/prepare?storage_id=primary', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ items: [
        { path: 'folder/one.txt', size: 11 },
        { path: 'folder/two.txt', size: 22 },
      ] }),
    }))
  })

  it('aborts an active upload through its cancellation signal', async () => {
    class AbortableXMLHttpRequest {
      static instances: AbortableXMLHttpRequest[] = []
      status = 0
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      upload = { addEventListener: () => undefined }
      constructor() { AbortableXMLHttpRequest.instances.push(this) }
      open(): void {}
      setRequestHeader(): void {}
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void {}
      abort(): void { this.listeners.get('abort')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', AbortableXMLHttpRequest)
    const controller = new AbortController()
    const result = uploadFile('file.txt', new File(['data'], 'file.txt'), () => undefined, 'primary', 'ticket', controller.signal)
    controller.abort()

    await expect(result).rejects.toThrow('上传已取消')
    expect(AbortableXMLHttpRequest.instances).toHaveLength(1)
  })

  it('releases a temporary upload batch without touching stored files', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ success: true }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await cancelUploadBatch('batch-ticket', 'primary')

    expect(fetchMock).toHaveBeenCalledWith('/api/upload/cancel?storage_id=primary', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ ticket: 'batch-ticket' }),
    }))
  })

  it('queries a bound upload result and can cancel only selected paths', async () => {
    const status = {
      ticket: 'batch-ticket',
      items: [{ path: 'folder/one.txt', size: 11, status: 'complete' }],
    }
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(status), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ success: true }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
    vi.stubGlobal('fetch', fetchMock)

    await expect(getUploadBatchStatus('batch-ticket', 'primary')).resolves.toEqual(status)
    await cancelUploadBatch('batch-ticket', 'primary', ['folder/two.txt'])

    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/upload/status?batch=batch-ticket&storage_id=primary', expect.objectContaining({ credentials: 'same-origin' }))
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/upload/cancel?storage_id=primary', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ ticket: 'batch-ticket', paths: ['folder/two.txt'] }),
    }))
  })

  it('preserves per-item details from a 207 batch response', async () => {
    const payload = {
      success: 1,
      failed: 1,
      results: [
        { path: 'one.txt', status: 200, code: 'ok', message: 'Completed' },
        { path: 'two.txt', status: 409, code: 'conflict', message: 'Target exists' },
      ],
    }
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(payload), {
      status: 207,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await expect(batchOperation('move', ['one.txt', 'two.txt'], '/target/')).resolves.toEqual(payload)
    expect(fetchMock).toHaveBeenCalledWith('/api/batch/move', expect.objectContaining({
      method: 'PUT',
      body: JSON.stringify({ paths: ['one.txt', 'two.txt'], target: '/target' }),
    }))
  })
})

describe('mutation result transport', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('keeps upload commit metadata and treats lost responses as unknown', async () => {
    for (const event of ['load', 'error']) {
      class ResultRequest {
        status = 409
        withCredentials = false
        responseText = JSON.stringify({ error: { code: 'operation_committed_pending', message: '已提交，请核对', operation: { commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat' } } })
        upload = { addEventListener: vi.fn() }
        listeners = new Map<string, () => void>()
        open(): void {}
        setRequestHeader(): void {}
        getResponseHeader(): null { return null }
        addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
        send(): void { queueMicrotask(() => this.listeners.get(event)?.()) }
        abort(): void {}
      }
      vi.stubGlobal('XMLHttpRequest', ResultRequest)
      await expect(uploadFile('note.txt', new File(['note'], 'note.txt'), () => undefined)).rejects.toMatchObject({
        code: event === 'load' ? 'operation_committed_pending' : 'operation_result_unknown',
        blocksRetry: true,
      })
    }
  })

  it('does not interpret a lost batch response as a confirmed failure', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('network')))
    await expect(batchOperation('copy', ['one.txt'], 'target')).rejects.toMatchObject({ code: 'operation_result_unknown', blocksRetry: true })
  })
})

describe('file size formatting', () => {
  it('uses stable binary units', () => {
    expect(formatSize(12)).toBe('12 B')
    expect(formatSize(1024)).toBe('1.0 KB')
    expect(formatSize(1024 ** 3)).toBe('1.0 GB')
  })
})

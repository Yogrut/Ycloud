import { afterEach, describe, expect, it, vi } from 'vitest'
import { batchOperation, createFolder, downloadUrl, fileApi, prepareArchive } from './browser'
import { formatSize } from '../format'

describe('browser API paths', () => {
  afterEach(() => vi.unstubAllGlobals())

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

describe('file size formatting', () => {
  it('uses stable binary units', () => {
    expect(formatSize(12)).toBe('12 B')
    expect(formatSize(1024)).toBe('1.0 KB')
    expect(formatSize(1024 ** 3)).toBe('1.0 GB')
  })
})

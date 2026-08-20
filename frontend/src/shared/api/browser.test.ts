import { afterEach, describe, expect, it, vi } from 'vitest'
import { createFolder, fileApi } from './browser'
import { formatSize } from '../format'

describe('browser API paths', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('keeps the root endpoint minimal', () => {
    expect(fileApi('')).toBe('/api/files')
    expect(fileApi('/')).toBe('/api/files')
  })

  it('encodes complete nested paths as one query value', () => {
    expect(fileApi('/中文/space name/')).toBe('/api/files?path=%2F%E4%B8%AD%E6%96%87%2Fspace%20name')
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
})

describe('file size formatting', () => {
  it('uses stable binary units', () => {
    expect(formatSize(12)).toBe('12 B')
    expect(formatSize(1024)).toBe('1.0 KB')
    expect(formatSize(1024 ** 3)).toBe('1.0 GB')
  })
})

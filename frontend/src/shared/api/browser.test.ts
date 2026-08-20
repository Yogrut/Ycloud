import { describe, expect, it } from 'vitest'
import { fileApi } from './browser'
import { formatSize } from '../format'

describe('browser API paths', () => {
  it('keeps the root endpoint minimal', () => {
    expect(fileApi('')).toBe('/api/files')
    expect(fileApi('/')).toBe('/api/files')
  })

  it('encodes complete nested paths as one query value', () => {
    expect(fileApi('/中文/space name/')).toBe('/api/files?path=%2F%E4%B8%AD%E6%96%87%2Fspace%20name')
  })
})

describe('file size formatting', () => {
  it('uses stable binary units', () => {
    expect(formatSize(12)).toBe('12 B')
    expect(formatSize(1024)).toBe('1.0 KB')
    expect(formatSize(1024 ** 3)).toBe('1.0 GB')
  })
})

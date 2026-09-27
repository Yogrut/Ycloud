import { afterEach, describe, expect, it, vi } from 'vitest'
import { uploadDirectFile } from './directUpload'

class StorageRequest extends EventTarget {
  static active = 0
  static peak = 0
  static payloads: Blob[] = []
  static failing = false
  upload = new EventTarget()
  withCredentials = true
  status = 200
  finished = false
  open = vi.fn()
  send(bytes: Blob): void {
    expect(this.withCredentials).toBe(false)
    StorageRequest.payloads.push(bytes)
    StorageRequest.peak = Math.max(StorageRequest.peak, ++StorageRequest.active)
    setTimeout(() => {
      if (this.finished) return
      this.finished = true
      StorageRequest.active--
      if (StorageRequest.failing) this.dispatchEvent(new Event('error'))
      else {
        this.upload.dispatchEvent(new ProgressEvent('progress', { loaded: bytes.size, total: bytes.size, lengthComputable: true }))
        this.dispatchEvent(new Event('load'))
      }
    }, 0)
  }
  abort(): void {
    if (this.finished) return
    this.finished = true
    StorageRequest.active--
    this.dispatchEvent(new Event('abort'))
  }
}

function install(mode = 'direct'): ReturnType<typeof vi.fn> {
  StorageRequest.active = 0
  StorageRequest.peak = 0
  StorageRequest.payloads = []
  StorageRequest.failing = false
  vi.stubGlobal('XMLHttpRequest', StorageRequest)
  const fetchMock = vi.fn(async (url: string) => {
    const action = new URL(url, 'http://localhost').pathname.split('/').at(-1)
    const body = action === 'start' ? { mode, session: 'session', part_size: 4, part_count: 5, concurrency: 4 }
      : action === 'part' ? { url: 'https://storage.example.com/bucket/temporary?signature=temporary' }
        : { success: true }
    return new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } })
  })
  vi.stubGlobal('fetch', fetchMock)
  return fetchMock
}

afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks() })

describe('S3 browser direct upload', () => {
  it('sends four parts directly, keeps progress accurate and completes once', async () => {
    const fetchMock = install()
    const progress = vi.fn()
    expect(await uploadDirectFile('?storage_id=s3&path=test&batch=ticket', new File(['abcdefghijklmnopqr'], 'test'), progress)).toBe(true)
    expect(StorageRequest.peak).toBe(4)
    expect(StorageRequest.payloads.map(part => part.size)).toEqual([4, 4, 4, 4, 2])
    expect(progress).toHaveBeenLastCalledWith(18)
    expect(fetchMock.mock.calls.filter(([url]) => url.includes('/complete'))).toHaveLength(1)
    expect(fetchMock.mock.calls.filter(([url]) => url.includes('/cancel'))).toHaveLength(0)
    expect(fetchMock.mock.calls.every(([url]) => url.startsWith('/api/upload/direct/'))).toBe(true)
  })
  it('uses the explicit relay response without sending storage PUTs', async () => {
    install('relay')
    expect(await uploadDirectFile('', new File(['a'], 'test'), vi.fn())).toBe(false)
    expect(StorageRequest.payloads).toHaveLength(0)
  })
  it('settles other PUTs and cancels the server session after a part fails', async () => {
    const fetchMock = install()
    StorageRequest.failing = true
    await expect(uploadDirectFile('', new File(['abcdefghijklmnopqr'], 'test'), vi.fn())).rejects.toMatchObject({ code: 'operation_result_unknown' })
    expect(StorageRequest.active).toBe(0)
    expect(fetchMock.mock.calls.filter(([url]) => url.includes('/complete'))).toHaveLength(0)
    expect(fetchMock.mock.calls.filter(([url]) => url.includes('/cancel'))).toHaveLength(1)
  })
})

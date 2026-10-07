import { ApiError, REQUEST_TIMEOUT_MS } from './client'
import { apiRequest } from './browser'
import { useLocale } from '../i18n'

interface DirectSession {
  mode: 'direct' | 'relay'
  session?: string
  part_size?: number
  part_count?: number
  concurrency?: number
}

function stalled(): ApiError {
  return new ApiError(useLocale().text('S3 直传失败，请核对任务状态，并检查存储地址、HTTPS 和跨域配置', 'S3 direct upload failed. Verify task status and check the storage endpoint, HTTPS and CORS configuration.'), 0, 'operation_result_unknown')
}

function putPart(url: string, bytes: Blob, progress: (loaded: number) => void, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest()
    const abort = () => request.abort()
    let timer: ReturnType<typeof setTimeout> | undefined
    const cleanup = () => { clearTimeout(timer); signal.removeEventListener('abort', abort) }
    const tick = () => {
      clearTimeout(timer)
      timer = setTimeout(() => { cleanup(); reject(stalled()); request.abort() }, REQUEST_TIMEOUT_MS)
    }
    const target = new URL(url)
    if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password) {
      reject(stalled()); return
    }
    request.open('PUT', url)
    // No Ycloud cookies, access tokens or S3 secret are attached to storage requests.
    request.withCredentials = false
    request.upload.addEventListener('progress', event => { tick(); progress(Math.min(bytes.size, event.loaded)) })
    request.addEventListener('load', () => {
      cleanup()
      if (request.status >= 200 && request.status < 300) { progress(bytes.size); resolve() }
      else reject(stalled())
    })
    request.addEventListener('error', () => { cleanup(); reject(stalled()) })
    request.addEventListener('abort', () => { cleanup(); reject(new DOMException('Upload aborted', 'AbortError')) })
    signal.addEventListener('abort', abort, { once: true })
    if (signal.aborted) { cleanup(); reject(new DOMException('Upload aborted', 'AbortError')); return }
    tick()
    request.send(bytes)
  })
}

export async function uploadDirectFile(
  query: string, file: File, onProgress: (loaded: number) => void, signal?: AbortSignal,
): Promise<boolean> {
  const command = <T>(action: string, body: unknown, requestSignal?: AbortSignal) => apiRequest<T>(`/api/upload/direct/${action}${query}`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal: requestSignal,
  })
  const controller = new AbortController()
  const abort = () => controller.abort()
  signal?.addEventListener('abort', abort, { once: true })
  if (signal?.aborted) controller.abort()
  let session: string | undefined
  try {
    const created = await command<DirectSession>('start', { size: file.size }, controller.signal)
    if (created.mode === 'relay') return false
    session = created.session
    const size = created.part_size ?? 0
    const count = created.part_count ?? 0
    if (!session || !Number.isSafeInteger(size) || size <= 0 || !Number.isSafeInteger(count)
      || count < 1 || count > 10_000 || count !== Math.ceil(Math.max(file.size, 1) / size)) throw stalled()
    const loaded = new Array<number>(count).fill(0)
    let totalLoaded = 0
    let next = 0
    const concurrency = Math.min(4, Math.max(1, created.concurrency ?? 1), count)
    // All workers are settled before cancellation, so no late PUT survives local termination.
    const results = await Promise.allSettled(Array.from({ length: concurrency }, async () => {
      try {
        while (next < count) {
          if (controller.signal.aborted) throw new DOMException('Upload aborted', 'AbortError')
          const index = next++
          const signed = await command<{ url: string }>('part', { session, part: index + 1 }, controller.signal)
          await putPart(signed.url, file.slice(index * size, Math.min(file.size, (index + 1) * size)), bytes => {
            totalLoaded += bytes - loaded[index]!
            loaded[index] = bytes
            onProgress(Math.min(file.size, totalLoaded))
          }, controller.signal)
        }
      } catch (error) { controller.abort(); throw error }
    }))
    const failure = results.find(result => result.status === 'rejected')
    if (failure?.status === 'rejected') throw failure.reason
    if (controller.signal.aborted) throw new DOMException('Upload aborted', 'AbortError')
    await command('complete', { session }, controller.signal)
    onProgress(file.size)
    return true
  } catch (error) {
    controller.abort()
    // Always send cancellation independently of the aborted data-transfer signal.
    // The existing batch-state reconciliation remains the authority on commit results.
    if (session) await command('cancel', { session }).catch(() => undefined)
    if (signal?.aborted || error instanceof ApiError && error.operation) throw error
    throw stalled()
  } finally { signal?.removeEventListener('abort', abort) }
}

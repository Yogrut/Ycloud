import { ApiError, requestWithDeadline } from '../../shared/api/client'

export interface GalleryImageResource {
  source: string
  release: () => void
}

export function fetchImageBlob(url: string, signal: AbortSignal, priority: 'low' | 'high'): Promise<Blob> {
  return requestWithDeadline(url, { signal, priority, cache: 'no-store' }, async response => {
    if (!response.ok) throw new ApiError('Image request failed', response.status)
    return response.blob()
  })
}

export async function decodeImageBlob(blob: Blob, signal: AbortSignal): Promise<GalleryImageResource> {
  signal.throwIfAborted()
  const source = URL.createObjectURL(blob)
  const image = new Image()
  let released = false
  function release(): void {
    if (released) return
    released = true
    signal.removeEventListener('abort', release)
    image.removeAttribute('src')
    URL.revokeObjectURL(source)
  }
  signal.addEventListener('abort', release, { once: true })
  try {
    image.decoding = 'async'
    image.src = source
    await image.decode()
    signal.throwIfAborted()
    return { source, release }
  } catch (error) {
    release()
    throw error
  }
}

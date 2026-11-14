import { ApiError, errorMetadata, readJson, requestWithDeadline } from '../../shared/api/client'

interface GalleryImageResource {
  source: string
  image: HTMLImageElement
  release: () => void
}

export function fetchImageBlob(url: string, signal: AbortSignal): Promise<Blob> {
  return requestWithDeadline(url, { signal, priority: 'high', cache: 'no-store' }, async response => {
    if (!response.ok) {
      const detail = errorMetadata(response, await readJson(response))
      throw new ApiError(detail.message ?? 'Image request failed', response.status, detail.code)
    }
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
    return { source, image, release }
  } catch (error) {
    release()
    throw error
  }
}

export interface GalleryThumbnail {
  bytes: Blob
  ratio: number
}

export type ThumbnailStatus = 'pending' | 'invalid' | 'limited' | 'unavailable' | 'traffic' | 'forbidden' | 'network'
export type ThumbnailResult = { status: 'ready'; thumb: GalleryThumbnail } | { status: ThumbnailStatus }

// Ready thumbnails carry a short private Cache-Control, so a repeat request may
// be answered from the browser cache instead of the network.
export function fetchGalleryThumbnail(url: string, signal: AbortSignal): Promise<ThumbnailResult> {
  return requestWithDeadline(url, { signal, priority: 'low' }, async response => {
    switch (response.status) {
      case 202: return { status: 'pending' }
      case 401:
      case 403:
      case 404: return { status: 'forbidden' }
      case 413: return { status: 'limited' }
      case 415: return { status: 'invalid' }
      case 422:
      case 503: return { status: 'unavailable' }
      case 429: return { status: errorMetadata(response, await readJson(response)).code === 'traffic_exhausted' ? 'traffic' : 'unavailable' }
      case 200: {
        const bytes = await response.blob()
        const resource = await decodeImageBlob(bytes, signal).catch(error => {
          if (signal.aborted) throw error
          return undefined
        })
        if (!resource) return { status: 'unavailable' }
        try {
          return { status: 'ready', thumb: { bytes, ratio: resource.image.naturalWidth / resource.image.naturalHeight } }
        } finally {
          resource.release()
        }
      }
      default: return { status: 'network' }
    }
  }, 15_000)
}

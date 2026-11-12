import { afterEach, describe, expect, it, vi } from 'vitest'
import { fetchGalleryThumbnail } from './galleryImageResource'

afterEach(() => vi.unstubAllGlobals())

describe('gallery thumbnail responses', () => {
  it.each([
    [202, undefined, 'pending'],
    [403, 'forbidden', 'forbidden'],
    [413, undefined, 'limited'],
    [415, undefined, 'invalid'],
    [422, undefined, 'unavailable'],
    [429, 'traffic_exhausted', 'traffic'],
    [429, 'too_many_requests', 'unavailable'],
    [503, 'service_unavailable', 'unavailable'],
  ])('keeps status %s (%s) separate as %s', async (status, code, expected) => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(
      code ? JSON.stringify({ error: { code } }) : null,
      { status },
    )))
    await expect(fetchGalleryThumbnail('/api/thumbnail?path=%2Fphoto.jpg', new AbortController().signal))
      .resolves.toEqual({ status: expected })
  })
})

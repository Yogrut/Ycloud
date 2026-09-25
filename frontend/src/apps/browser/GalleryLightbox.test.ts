import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import GalleryLightbox from './GalleryLightbox.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('GalleryLightbox', () => {
  it('explains exhausted preview traffic without suggesting a download', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 429 }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryLightbox, {
      entry: { name: 'photo.jpg', path: 'photo.jpg', is_dir: false, size: 1024, modified: '', mime: 'image/jpeg', icon: 'image', locked: false },
      storageId: 'primary',
      index: 0,
      total: 1,
    })
    app.mount(host)
    await nextTick()
    document.body.querySelector('img')!.dispatchEvent(new Event('error'))
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    const fallback = document.body.querySelector('.gallery-lightbox-unavailable')!
    expect(fallback.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(fallback.querySelector('a')).toBeNull()
    expect(fetchMock).toHaveBeenCalledWith(expect.stringContaining('/api/preview?'), expect.objectContaining({ method: 'HEAD' }))
    app.unmount()
  })
})

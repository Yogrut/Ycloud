import { createApp, h, nextTick, reactive, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import GalleryLightbox from './GalleryLightbox.vue'

const mountedApps: Array<ReturnType<typeof createApp>> = []

function showGallery() {
  const entry = reactive({ name: 'photo.jpg', path: 'photo.jpg', is_dir: false, size: 1024, modified: '', mime: 'image/jpeg', icon: 'image', locked: false })
  const storageId = ref('primary')
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp({ render: () => h(GalleryLightbox, { entry, storageId: storageId.value, index: 0, total: 1 }) })
  mountedApps.push(app)
  app.mount(host)
  return { app, entry, storageId }
}

afterEach(() => {
  mountedApps.splice(0).forEach(app => app.unmount())
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('GalleryLightbox', () => {
  it('explains exhausted preview traffic without suggesting a download', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 429 }))
    vi.stubGlobal('fetch', fetchMock)
    showGallery()
    await nextTick()
    document.body.querySelector('img')!.dispatchEvent(new Event('error'))
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    const fallback = document.body.querySelector('.gallery-lightbox-unavailable')!
    expect(fallback.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(fallback.querySelector('a')).toBeNull()
    expect(fetchMock).toHaveBeenCalledWith(expect.stringContaining('/api/preview?'), expect.objectContaining({ method: 'HEAD' }))
  })

  it('ignores a quota check from an earlier opening of the same image', async () => {
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const { entry } = showGallery()
    document.body.querySelector('img')!.dispatchEvent(new Event('error'))
    entry.path = 'other.jpg'
    entry.path = 'photo.jpg'
    await nextTick()
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null, { status: 429 }))
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('img')).not.toBeNull()
    expect(document.body.querySelector('.gallery-lightbox-unavailable')).toBeNull()
  })

  it('starts only one quota diagnosis for repeated image errors', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    showGallery()
    const image = document.body.querySelector('img')!
    image.dispatchEvent(new Event('error'))
    image.dispatchEvent(new Event('error'))
    expect(fetchMock).toHaveBeenCalledOnce()
    await nextTick()
  })

  it('resets image failure and zoom when the same path changes storage', async () => {
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const { storageId } = showGallery()
    document.body.querySelector<HTMLImageElement>('img')!.dispatchEvent(new Event('dblclick'))
    await nextTick()
    expect(document.body.querySelector('.gallery-zoom-value')?.textContent).toBe('200%')
    document.body.querySelector('img')!.dispatchEvent(new Event('error'))
    storageId.value = 'secondary'
    await nextTick()
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null, { status: 429 }))
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('img')?.getAttribute('src')).toContain('storage_id=secondary')
    expect(document.body.querySelector('.gallery-zoom-value')?.textContent).toBe('100%')
    expect(document.body.querySelector('.gallery-lightbox-unavailable')).toBeNull()
  })

  it('deduplicates fallback downloads and cancels them when changing storage', async () => {
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockResolvedValueOnce(new Response(null, { status: 404 }))
      .mockReturnValueOnce(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
    const { storageId } = showGallery()
    document.body.querySelector('img')!.dispatchEvent(new Event('error'))
    await new Promise(accept => setTimeout(accept, 0))
    const link = document.body.querySelector<HTMLAnchorElement>('.gallery-lightbox-unavailable a')!
    link.click()
    link.click()
    expect(fetchMock).toHaveBeenCalledTimes(2)
    storageId.value = 'secondary'
    await nextTick()
    expect(fetchMock.mock.calls[1]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null))
    await new Promise(accept => setTimeout(accept, 0))
    expect(navigate).not.toHaveBeenCalled()
    expect(document.body.querySelector('.gallery-lightbox-unavailable')).toBeNull()
  })
})

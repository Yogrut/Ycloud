import { createApp, h, nextTick, reactive, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import GalleryLightbox from './GalleryLightbox.vue'

const mountedApps: Array<ReturnType<typeof createApp>> = []
const requests: { url: string; signal: AbortSignal; resolve: (response: Response) => void }[] = []
const settle = () => new Promise(resolve => setTimeout(resolve, 0))
const image = () => document.body.querySelector<HTMLImageElement>('.gallery-lightbox-stage img')

function showGallery(withThumbnail = true) {
  const entry = reactive({ name: 'photo.jpg', path: 'photo.jpg', is_dir: false, size: 1024, modified: '', mime: 'image/jpeg', icon: 'image', locked: false })
  const storageId = ref('primary')
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp({ render: () => h(GalleryLightbox, {
    entry, storageId: storageId.value, index: 0, total: 2,
    thumbnailSource: withThumbnail ? `blob:thumb-${storageId.value}-${entry.path}` : undefined,
  }) })
  mountedApps.push(app)
  app.mount(host)
  return { app, entry, storageId }
}

function close(app: ReturnType<typeof createApp>): void {
  mountedApps.splice(mountedApps.indexOf(app), 1)
  app.unmount()
}

beforeEach(() => {
  requests.length = 0
  vi.stubGlobal('fetch', vi.fn((url: string, options: RequestInit) => new Promise<Response>(resolve => {
    requests.push({ url, signal: options.signal!, resolve })
  })))
  vi.stubGlobal('Image', class {
    src = ''
    decode() { return Promise.resolve() }
    removeAttribute() { this.src = '' }
  })
  let sequence = 0
  vi.spyOn(URL, 'createObjectURL').mockImplementation(() => `blob:original-${++sequence}`)
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {})
})

afterEach(() => {
  mountedApps.splice(0).forEach(app => app.unmount())
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('GalleryLightbox', () => {
  it('shows the existing thumbnail immediately and upgrades it without resetting zoom', async () => {
    showGallery()
    expect(image()?.getAttribute('src')).toBe('blob:thumb-primary-photo.jpg')
    expect(requests.map(request => request.url)).toEqual(['/api/preview?path=%2Fphoto.jpg&storage_id=primary'])
    expect(fetch).toHaveBeenCalledWith(expect.any(String), expect.objectContaining({ cache: 'no-store', priority: 'high' }))
    expect(document.body.querySelector('[role="status"]')?.textContent).toContain('正在加载原图')
    const thumb = image()
    image()!.dispatchEvent(new Event('dblclick'))
    requests[0]!.resolve(new Response('image bytes'))
    await settle()
    expect(image()).toBe(thumb)
    expect(image()?.getAttribute('src')).toBe('blob:original-1')
    expect(document.body.querySelector('.gallery-zoom-value')?.textContent).toBe('200%')
    expect(document.body.querySelector('[role="status"]')).toBeNull()
  })

  it('releases the old original on switch and the current original on close', async () => {
    const { app, entry } = showGallery()
    requests[0]!.resolve(new Response('first'))
    await settle()
    const old = image()
    entry.path = 'next.jpg'
    await nextTick()
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:original-1')
    expect(old?.getAttribute('src')).toBeNull()
    expect(image()?.getAttribute('src')).toBe('blob:thumb-primary-next.jpg')
    requests[1]!.resolve(new Response('second'))
    await settle()
    expect(image()?.getAttribute('src')).toBe('blob:original-2')
    close(app)
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:original-2')
    expect(URL.revokeObjectURL).toHaveBeenCalledTimes(2)
  })

  it('cancels pending originals on rapid A to B to A navigation and ignores stale responses', async () => {
    const { app, entry } = showGallery(false)
    entry.path = 'next.jpg'
    entry.path = 'photo.jpg'
    expect(requests).toHaveLength(3)
    expect(requests.slice(0, 2).every(request => request.signal.aborted)).toBe(true)
    requests[0]!.resolve(new Response(null, { status: 429 }))
    requests[1]!.resolve(new Response('stale original'))
    await settle()
    expect(image()).toBeNull()
    expect(document.body.querySelector('.gallery-lightbox-unavailable')).toBeNull()
    expect(URL.createObjectURL).not.toHaveBeenCalled()
    close(app)
    expect(requests[2]!.signal.aborted).toBe(true)
    requests[2]!.resolve(new Response('closed original'))
    await settle()
    expect(URL.createObjectURL).not.toHaveBeenCalled()
  })

  it('keeps an authorized thumbnail if the original is quota-denied, without a second request', async () => {
    showGallery()
    requests[0]!.resolve(new Response(JSON.stringify({ error: { code: 'traffic_exhausted' } }), { status: 429 }))
    await settle()
    expect(image()?.getAttribute('src')).toBe('blob:thumb-primary-photo.jpg')
    const unavailable = document.body.querySelector('.gallery-lightbox-unavailable')!
    expect(unavailable.textContent).toContain('下载流量已用尽')
    expect(unavailable.querySelector('a')).toBeNull()
    expect(requests).toHaveLength(1)
  })

  it('clears a thumbnail when the original is no longer authorized', async () => {
    showGallery()
    requests[0]!.resolve(new Response(null, { status: 403 }))
    await settle()
    expect(image()).toBeNull()
  })

  it('deduplicates fallback downloads and cancels them when changing storage', async () => {
    const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
    const { storageId } = showGallery()
    requests[0]!.resolve(new Response(null, { status: 404 }))
    await settle()
    const link = document.body.querySelector<HTMLAnchorElement>('.gallery-lightbox-unavailable a')!
    link.click()
    link.click()
    expect(requests).toHaveLength(2)
    storageId.value = 'secondary'
    await nextTick()
    expect(requests[1]!.signal.aborted).toBe(true)
    expect(requests[2]!.url).toContain('storage_id=secondary')
    requests[1]!.resolve(new Response(null))
    await settle()
    expect(navigate).not.toHaveBeenCalled()
    expect(document.body.querySelector('.gallery-lightbox-unavailable')).toBeNull()
  })

  it('coalesces pointer samples into one transform update per frame and cancels on disposal', async () => {
    const frames = new Map<number, FrameRequestCallback>()
    let sequence = 0
    const schedule = vi.fn((callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence })
    const cancel = vi.fn((id: number) => frames.delete(id))
    vi.stubGlobal('requestAnimationFrame', schedule)
    vi.stubGlobal('cancelAnimationFrame', cancel)
    const { app } = showGallery()
    await nextTick()
    const image = document.body.querySelector<HTMLImageElement>('img')!
    image.dispatchEvent(new MouseEvent('pointerdown', { button: 0, clientX: 10, clientY: 10 }))
    for (let i = 0; i < 100; i++) image.dispatchEvent(new MouseEvent('pointermove', { clientX: 20 + i, clientY: 30 + i }))
    await nextTick()
    expect(schedule).toHaveBeenCalledTimes(1)
    expect(image.style.transform).toBe('translate(0px, 0px) scale(1)')
    frames.get(1)!(0)
    expect(image.style.transform).toBe('translate(109px, 119px) scale(1)')
    image.dispatchEvent(new MouseEvent('pointermove', { clientX: 20, clientY: 30 }))
    expect(schedule).toHaveBeenCalledTimes(2)
    close(app)
    expect(cancel).toHaveBeenCalledWith(2)
    expect(frames.size).toBe(0)
  })
})

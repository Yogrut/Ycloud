/* eslint-disable vue/one-component-per-file */
import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { FileEntry } from '../../shared/api/browser'
import GalleryGrid from './GalleryGrid.vue'
import GalleryLightbox from './GalleryLightbox.vue'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })

function entry(name: string): FileEntry {
  return { name, path: `photos/${name}`, is_dir: false, size: 12, modified: '', mime: 'image/*', icon: 'image', locked: false }
}

describe('gallery presentation', () => {
  it('shows only the six supported browser image formats in source order', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryGrid, {
      entries: [
        entry('one.JPG'), entry('two.jpeg'), entry('three.png'), entry('four.webp'), entry('five.avif'), entry('six.gif'),
        entry('no.bmp'), entry('no.ico'), entry('no.svg'), entry('no.tiff'), entry('no.jxl'), entry('no.heic'), entry('no.raw'),
      ],
      storageId: 'primary',
    })
    app.mount(host)
    await nextTick()

    const cards = [...host.querySelectorAll<HTMLElement>('.gallery-card')]
    expect(cards).toHaveLength(6)
    expect(cards.map(card => card.querySelector('.gallery-card-open')?.getAttribute('aria-label'))).toEqual(['one.JPG', 'two.jpeg', 'three.png', 'four.webp', 'five.avif', 'six.gif'])
    expect(host.querySelectorAll('.gallery-select')).toHaveLength(6)
    expect(host.textContent).not.toContain('no.bmp')
    expect(host.textContent).not.toContain('one.JPG')
    expect(host.querySelector('.gallery-card-caption')).toBeNull()
    expect(host.querySelector('img')?.getAttribute('src')).toContain('/api/preview?path=%2Fphotos%2Fone.JPG&storage_id=primary')
    app.unmount()
  })

  it('selects an image without opening its preview', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const opened = vi.fn()
    const selected = vi.fn()
    const app = createApp(GalleryGrid, { entries: [entry('one.jpg')], storageId: 'primary', selected: new Set(['photos/one.jpg']), onOpen: opened, onSelect: selected })
    app.mount(host)
    await nextTick()

    const circle = host.querySelector<HTMLButtonElement>('.gallery-select')!
    expect(circle.getAttribute('aria-pressed')).toBe('true')
    circle.click()
    expect(selected).toHaveBeenCalledWith(expect.objectContaining({ path: 'photos/one.jpg' }))
    expect(opened).not.toHaveBeenCalled()
    app.unmount()
  })

  it('sizes gallery images from their natural aspect ratios', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryGrid, { entries: [entry('wide.jpg'), entry('tall.jpg')], storageId: 'primary' })
    app.mount(host)
    await nextTick()

    const wide = host.querySelectorAll<HTMLImageElement>('.gallery-card img')[0]!
    const tall = host.querySelectorAll<HTMLImageElement>('.gallery-card img')[1]!
    Object.defineProperties(wide, { naturalWidth: { value: 400 }, naturalHeight: { value: 200 } })
    Object.defineProperties(tall, { naturalWidth: { value: 100 }, naturalHeight: { value: 200 } })
    wide.dispatchEvent(new Event('load'))
    tall.dispatchEvent(new Event('load'))
    await nextTick()

    const cards = [...host.querySelectorAll<HTMLElement>('.gallery-card')]
    expect(cards[0]?.style.width).toBe('380px')
    expect(cards[1]?.style.width).toBe('95px')
    expect(cards[0]?.style.height).toBe('190px')
    app.unmount()
  })

  it('supports keyboard navigation, closing, and zooming in the lightbox', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const events: string[] = []
    const app = createApp(GalleryLightbox, {
      entry: entry('one.jpg'),
      storageId: 'primary',
      index: 1,
      total: 3,
      onPrevious: () => events.push('previous'),
      onNext: () => events.push('next'),
      onClose: () => events.push('close'),
    })
    app.mount(host)
    await nextTick()

    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft' }))
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight' }))
    document.dispatchEvent(new KeyboardEvent('keydown', { key: '+' }))
    await nextTick()
    expect(document.body.querySelector('.gallery-zoom-value')?.textContent).toBe('125%')
    document.dispatchEvent(new KeyboardEvent('keydown', { key: '-' }))
    await nextTick()
    expect(document.body.querySelector('.gallery-zoom-value')?.textContent).toBe('100%')
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
    expect(events).toEqual(['previous', 'next', 'close'])
    app.unmount()
  })

  it('lets a zoomed image be dragged and resets its position with the zoom', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryLightbox, {
      entry: entry('detail.jpg'),
      storageId: 'primary',
      index: 0,
      total: 1,
    })
    app.mount(host)
    await nextTick()

    const image = document.body.querySelector<HTMLImageElement>('.gallery-lightbox-stage img')!
    image.setPointerCapture = () => undefined
    image.releasePointerCapture = () => undefined
    document.dispatchEvent(new KeyboardEvent('keydown', { key: '+' }))
    await nextTick()

    image.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 100, clientY: 120 }))
    image.dispatchEvent(new MouseEvent('pointermove', { bubbles: true, clientX: 145, clientY: 85 }))
    await nextTick()
    expect(image.style.transform).toBe('translate(45px, -35px) scale(1.25)')
    expect(image.classList.contains('is-panning')).toBe(true)

    image.dispatchEvent(new MouseEvent('pointerup', { bubbles: true }))
    document.body.querySelector<HTMLButtonElement>('.gallery-zoom-value')!.click()
    await nextTick()
    expect(image.style.transform).toBe('translate(0px, 0px) scale(1)')
    expect(image.classList.contains('is-panning')).toBe(false)
    app.unmount()
  })

  it('lets an image be dragged at 100% and below 100%', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryLightbox, { entry: entry('small.jpg'), storageId: 'primary', index: 0, total: 1 })
    app.mount(host)
    await nextTick()

    const image = document.body.querySelector<HTMLImageElement>('.gallery-lightbox-stage img')!
    image.setPointerCapture = () => undefined
    image.releasePointerCapture = () => undefined
    image.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 10, clientY: 10 }))
    image.dispatchEvent(new MouseEvent('pointermove', { bubbles: true, clientX: 30, clientY: 40 }))
    image.dispatchEvent(new MouseEvent('pointerup', { bubbles: true }))
    await nextTick()
    expect(image.style.transform).toBe('translate(20px, 30px) scale(1)')

    document.dispatchEvent(new KeyboardEvent('keydown', { key: '-' }))
    image.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, button: 0, clientX: 10, clientY: 10 }))
    image.dispatchEvent(new MouseEvent('pointermove', { bubbles: true, clientX: 0, clientY: 0 }))
    image.dispatchEvent(new MouseEvent('pointerup', { bubbles: true }))
    await nextTick()
    expect(image.style.transform).toBe('translate(10px, 20px) scale(0.75)')

    document.body.querySelector<HTMLButtonElement>('.gallery-zoom-value')!.click()
    await nextTick()
    expect(image.style.transform).toBe('translate(0px, 0px) scale(1)')
    app.unmount()
  })

  it('offers a download when the browser cannot display a gallery image', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 404 })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryLightbox, {
      entry: entry('broken.avif'),
      storageId: 'primary',
      index: 0,
      total: 1,
    })
    app.mount(host)
    await nextTick()

    document.body.querySelector('.gallery-lightbox-stage img')?.dispatchEvent(new Event('error'))
    await nextTick()
    expect(document.body.querySelector('.gallery-lightbox-stage img')).toBeNull()
    expect(document.body.querySelector('.gallery-lightbox-unavailable a')?.getAttribute('href'))
      .toContain('/api/download?path=%2Fphotos%2Fbroken.avif&storage_id=primary')
    expect(document.body.querySelector('.gallery-lightbox-toolbar')).toBeNull()
    app.unmount()
  })
})

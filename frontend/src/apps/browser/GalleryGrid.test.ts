/* eslint-disable vue/one-component-per-file */
import { createApp, h, nextTick, reactive } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { FileEntry } from '../../shared/api/browser'
import GalleryGrid from './GalleryGrid.vue'
import GalleryLightbox from './GalleryLightbox.vue'

afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren() })
beforeEach(() => { vi.stubGlobal('fetch', vi.fn(() => new Promise(() => {}))) })

function entry(name: string): FileEntry {
  return { name, path: `photos/${name}`, is_dir: false, size: 12, modified: '', mime: 'image/*', icon: 'image', locked: false }
}

describe('gallery presentation', () => {
  it('preserves source order when the number of vertically arranged images changes', async () => {
    const host = document.createElement('div')
    const props = reactive({ entries: Array.from({ length: 14 }, (_, i) => entry(`${i}.jpg`)), storageId: 'primary' })
    const app = createApp({ render: () => h(GalleryGrid, props) })
    app.mount(host)
    const paths = () => [...host.querySelectorAll<HTMLElement>('.gallery-card')].map(card => card.dataset.entryPath)
    expect(paths()).toEqual(props.entries.map(item => item.path))
    props.entries = Array.from({ length: 20 }, (_, i) => entry(`${i}.jpg`))
    await nextTick()
    expect(paths()).toEqual(props.entries.map(item => item.path))
    props.entries = props.entries.slice(0, 5)
    await nextTick()
    expect(paths()).toEqual(props.entries.map(item => item.path))
    app.unmount()
  })

  it('retains unchanged image elements after deletion and updates URLs when file versions change', async () => {
    const host = document.createElement('div')
    const entries = Array.from({ length: 10 }, (_, i) => entry(`${i}.jpg`))
    const props = reactive({ entries, storageId: 'primary' })
    const app = createApp({ render: () => h(GalleryGrid, props) })
    app.mount(host)
    const before = [...host.querySelectorAll<HTMLImageElement>('img')]
    props.entries = entries.slice(1)
    await nextTick()
    const after = [...host.querySelectorAll('img')]
    expect(after).toHaveLength(before.length - 1)
    expect(after.every((image, i) => image === before[i + 1])).toBe(true)
    const oldUrl = after[0]!.getAttribute('src')
    props.entries[0]!.modified = 'changed'
    await nextTick()
    expect(host.querySelector('img')!.getAttribute('src')).not.toBe(oldUrl)
    app.unmount()
  })

  it('uses browser-native lazy thumbnail URLs without downloading original blobs', async () => {
    const host = document.createElement('div')
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const entries = [entry('one.jpg'), entry('two.jpg')]
    const props = reactive({ entries, storageId: 'primary', canDownload: true })
    const app = createApp({ render: () => h(GalleryGrid, props) })
    app.mount(host)
    const loaded = host.querySelector<HTMLImageElement>('img')!
    expect(host.querySelectorAll('img')).toHaveLength(2)
    expect(loaded.getAttribute('src')).toContain('/api/thumbnail?')
    expect(loaded.getAttribute('loading')).toBe('lazy')
    expect(loaded.getAttribute('decoding')).toBe('async')
    loaded.dispatchEvent(new Event('load'))
    expect(loaded.classList.contains('is-loaded')).toBe(true)
    loaded.dispatchEvent(new Event('error'))
    expect(loaded.classList.contains('is-loaded')).toBe(false)
    props.canDownload = false
    await nextTick()
    expect(host.querySelector('img')).toBeNull()
    expect(fetchMock).not.toHaveBeenCalled()
    app.unmount()
  })

  it('shows browser image formats including JFIF in source order', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(GalleryGrid, {
      entries: [
        entry('one.JPG'), entry('two.jpeg'), entry('three.png'), entry('four.webp'), entry('five.avif'), entry('six.gif'), entry('seven.jfif'),
        entry('no.bmp'), entry('no.ico'), entry('no.svg'), entry('no.tiff'), entry('no.jxl'), entry('no.heic'), entry('no.raw'),
      ],
      storageId: 'primary',
    })
    app.mount(host)
    await nextTick()

    const cards = [...host.querySelectorAll<HTMLElement>('.gallery-card')]
    expect(cards).toHaveLength(7)
    expect(cards.map(card => card.querySelector('.gallery-card-open')?.getAttribute('aria-label'))).toEqual(['one.JPG', 'two.jpeg', 'three.png', 'four.webp', 'five.avif', 'six.gif', 'seven.jfif'])
    expect(host.querySelectorAll('.gallery-select')).toHaveLength(7)
    expect(host.textContent).not.toContain('no.bmp')
    expect(host.textContent).not.toContain('one.JPG')
    expect(host.querySelector('.gallery-card-caption')).toBeNull()
    expect(host.querySelector('img')?.getAttribute('src')).toContain('/api/thumbnail?path=%2Fphotos%2Fone.JPG')
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

  it('supports keyboard navigation, closing, and zooming in the lightbox', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const events: string[] = []
    const app = createApp(GalleryLightbox, {
      entry: entry('one.jpg'),
      storageId: 'primary',
      index: 1,
      total: 3,
      thumbnailSource: 'blob:thumbnail',
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
      thumbnailSource: 'blob:thumbnail',
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
    await new Promise(requestAnimationFrame)
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
    const app = createApp(GalleryLightbox, { entry: entry('small.jpg'), storageId: 'primary', index: 0, total: 1, thumbnailSource: 'blob:thumbnail' })
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

    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(document.body.querySelector('.gallery-lightbox-stage img')).toBeNull()
    expect(document.body.querySelector('.gallery-lightbox-unavailable a')?.getAttribute('href'))
      .toContain('/api/download?path=%2Fphotos%2Fbroken.avif&storage_id=primary')
    expect(document.body.querySelector('.gallery-lightbox-toolbar')).toBeNull()
    app.unmount()
  })
})

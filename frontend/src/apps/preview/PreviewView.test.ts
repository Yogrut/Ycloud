import { createApp, nextTick, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Window as TestWindow } from 'happy-dom'
import PreviewView from './PreviewView.vue'

const mountedApps: Array<ReturnType<typeof createApp>> = []
const iframeNavigation = (window as unknown as TestWindow).happyDOM.settings.navigation
const originalChildNavigation = iframeNavigation.disableChildFrameNavigation

beforeEach(() => { iframeNavigation.disableChildFrameNavigation = true })

function mountPreview(host: HTMLElement) {
  const app = createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } })
  mountedApps.push(app)
  app.mount(host)
  return app
}

afterEach(() => {
  mountedApps.splice(0).forEach(app => app.unmount())
  iframeNavigation.disableChildFrameNavigation = originalChildNavigation
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  Reflect.deleteProperty(navigator, 'pdfViewerEnabled')
  document.body.replaceChildren()
  window.history.replaceState(null, '', '/')
})

describe('PreviewView', () => {
  it.each(['movie.mkv', 'movie.mov', 'song.ogg', 'song.aac', 'image.bmp', 'image.ico', 'report.docx'])(
    'offers a download instead of previewing unsupported %s', async (filename) => {
      window.history.replaceState(null, '', `/preview?path=${encodeURIComponent(`/${filename}`)}`)
      const host = document.createElement('div')
      document.body.append(host)
      mountPreview(host)
      await nextTick()
      expect(host.querySelector('video, audio, img, iframe, pre')).toBeNull()
      expect(host.querySelector('.preview-message a')?.textContent).toBe('下载文件')
    },
  )

  it.each([
    ['photo.avif', 'img'],
    ['movie.mp4', 'video'],
    ['movie.webm', 'video'],
    ['song.mp3', 'audio'],
    ['song.m4a', 'audio'],
    ['song.wav', 'audio'],
    ['song.flac', 'audio'],
  ])('lets the browser try supported %s', async (filename, selector) => {
    window.history.replaceState(null, '', `/preview?path=${encodeURIComponent(`/${filename}`)}`)
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    expect(host.querySelector(selector)).not.toBeNull()
    if (selector === 'video' || selector === 'audio') {
      expect(host.querySelector(selector === 'video' ? '.ycloud-video-controls' : '.ycloud-audio-track')).not.toBeNull()
      expect(host.querySelector(selector)?.hasAttribute('controls')).toBe(false)
    }
  })

  it('offers a download when the browser PDF viewer is unavailable', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Freport.pdf')
    Object.defineProperty(navigator, 'pdfViewerEnabled', { configurable: true, value: false })
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    expect(host.querySelector('iframe')).toBeNull()
    expect(host.querySelector('.preview-message a')?.textContent).toBe('下载文件')
  })

  it('shows a PDF quota error before loading the embedded viewer', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Freport.pdf')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelector('iframe')).toBeNull()
    expect(host.querySelector('.preview-message')?.textContent).toContain('下载流量已用尽或剩余流量不足')
  })

  it('shows a quota error rather than a download link when image preview is denied', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Fphoto.png')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    host.querySelector('img')!.dispatchEvent(new Event('error'))
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelector('.preview-message')?.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(host.querySelector('.preview-message a')).toBeNull()
    expect(host.querySelector('.preview-actions a')).toBeNull()
  })

  it('shows the server quota message for a denied text document', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Fnotes.txt')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'traffic_exhausted' } }), { status: 429 })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelector('.preview-message')?.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(host.querySelector('.preview-message a')).toBeNull()
  })

  it.each([
    ['photo.png', 'img'],
    ['movie.mp4', 'video'],
    ['song.mp3', 'audio'],
  ])('offers a download when supported %s fails to load', async (filename, selector) => {
    window.history.replaceState(null, '', `/preview?path=${encodeURIComponent(`/${filename}`)}`)
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 404 })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    host.querySelector(selector)?.dispatchEvent(new Event('error'))
    await nextTick()
    expect(host.querySelector(selector)).toBeNull()
    expect(host.querySelector('.preview-message a')?.textContent).toBe('下载文件')
  })

  it('does not mark a complete ranged text response as truncated', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Fnote.txt')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('complete', {
      status: 206,
      headers: { 'Content-Range': 'bytes 0-7/8' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('pre')?.textContent).toBe('complete')
  })

  it('marks a genuinely partial text response as truncated', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Flarge.txt')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('partial', {
      status: 206,
      headers: { 'Content-Range': 'bytes 0-6/20' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('pre')?.textContent).toContain('[预览已截断，仅显示前 2 MiB]')
  })

  it('cancels text reading on unmount and drops its late quota response', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Fnotes.txt')
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPreview(host)
    app.unmount()
    mountedApps.splice(mountedApps.indexOf(app), 1)
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null, { status: 429 }))
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('.preview-page')).toBeNull()
    expect(document.body.textContent).not.toContain('下载流量已用尽')
  })

  it('keeps readable text visible if its separate download check fails', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Fnotes.txt')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(new Response('readable document')).mockResolvedValueOnce(new Response(null, { status: 403 })))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await new Promise(accept => setTimeout(accept, 0))
    host.querySelector<HTMLAnchorElement>('.preview-actions a')!.click()
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(host.querySelector('pre')?.textContent).toBe('readable document')
    expect(document.body.textContent).toContain('没有下载权限')
    expect(host.querySelector('.preview-message')).toBeNull()
  })

  it('caps a text body that ignores Range and shows truncation', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Flarge.txt')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('a'.repeat(2 * 1024 * 1024 + 1))))
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(host.querySelector('pre')?.textContent).toBe(`${'a'.repeat(2 * 1024 * 1024)}\n\n[预览已截断，仅显示前 2 MiB]`)
  })

  it('deduplicates header and fallback download clicks', () => {
    window.history.replaceState(null, '', '/preview?path=%2Farchive.zip')
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    host.querySelector<HTMLAnchorElement>('.preview-actions a')!.click()
    host.querySelector<HTMLAnchorElement>('.preview-message a')!.click()
    expect(fetchMock).toHaveBeenCalledOnce()
  })

  it.each([200, 403])('cancels pending download admission (%s) on unmount', async status => {
    window.history.replaceState(null, '', '/preview?path=%2Farchive.zip')
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPreview(host)
    host.querySelector<HTMLAnchorElement>('.preview-actions a')!.click()
    app.unmount()
    mountedApps.splice(mountedApps.indexOf(app), 1)
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null, { status }))
    await new Promise(accept => setTimeout(accept, 0))
    expect(navigate).not.toHaveBeenCalled()
    expect(document.body.textContent).not.toContain('没有下载权限')
  })

  it('cancels PDF admission when leaving the preview page', () => {
    window.history.replaceState(null, '', '/preview?path=%2Freport.pdf')
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountPreview(host)
    app.unmount()
    mountedApps.splice(mountedApps.indexOf(app), 1)
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
  })

  it('does not start a root download for a missing preview path', async () => {
    window.history.replaceState(null, '', '/preview')
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    mountPreview(host)
    host.querySelector<HTMLAnchorElement>('.preview-actions a')!.click()
    await nextTick()
    expect(fetchMock).not.toHaveBeenCalled()
  })
})

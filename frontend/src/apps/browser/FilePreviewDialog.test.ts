import { createApp, nextTick, reactive } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Window as TestWindow } from 'happy-dom'
import type { FileEntry } from '../../shared/api/browser'
import FilePreviewDialog from './FilePreviewDialog.vue'

const mountedApps: Array<ReturnType<typeof createApp>> = []
const iframeNavigation = (window as unknown as TestWindow).happyDOM.settings.navigation
const originalChildNavigation = iframeNavigation.disableChildFrameNavigation

// Test the viewer's DOM without starting a separate iframe network request.
beforeEach(() => { iframeNavigation.disableChildFrameNavigation = true })

function showFile(name: string) {
  const entry = reactive<FileEntry>({
    name,
    path: name,
    is_dir: false,
    size: 1024,
    modified: '2026-09-24 18:00',
    mime: '',
    icon: '',
    locked: false,
  })
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(FilePreviewDialog, { entry, storageId: '' })
  mountedApps.push(app)
  app.mount(host)
  return { app, entry }
}

afterEach(() => {
  mountedApps.splice(0).forEach(app => app.unmount())
  iframeNavigation.disableChildFrameNavigation = originalChildNavigation
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

describe('FilePreviewDialog', () => {
  it.each(['clip.mp4', 'notes.txt'])('focuses the %s dialog without initially outlining its close button', async name => {
    if (name.endsWith('.txt')) vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('hello', { status: 200 })))
    showFile(name)
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(document.activeElement).toBe(document.body.querySelector('.ycloud-preview-dialog'))
    expect(document.activeElement).not.toBe(document.body.querySelector('.ycloud-preview-close'))
  })

  it('puts custom video controls over the video instead of using native controls', async () => {
    showFile('clip.mp4')
    await nextTick()
    expect(document.body.querySelector('.ycloud-video-stage .ycloud-video-controls')).not.toBeNull()
    expect(document.body.querySelector('video')?.hasAttribute('controls')).toBe(false)
    expect(document.body.querySelector('.ycloud-preview-dialog.video')).not.toBeNull()
    expect(document.body.querySelector('.ycloud-video-actions .app-icon')).toBeNull()
    expect(document.body.querySelector('.ycloud-video-progress .ycloud-media-seek')).not.toBeNull()
  })

  it('keeps the custom video progress bar in sync with playback', async () => {
    showFile('clip.mp4')
    await nextTick()
    const video = document.body.querySelector('video')!
    Object.defineProperties(video, { duration: { configurable: true, value: 120 }, currentTime: { configurable: true, value: 30 } })
    video.dispatchEvent(new Event('timeupdate'))
    await nextTick()
    expect(document.body.querySelector<HTMLElement>('.ycloud-video-progress-fill')?.style.width).toBe('25%')
    expect(document.body.querySelector<HTMLInputElement>('.ycloud-video-progress input')?.value).toBe('30')
  })

  it('shows the quota error without a download suggestion when video preview gets 429', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    showFile('clip.mp4')
    await nextTick()
    document.body.querySelector('video')!.dispatchEvent(new Event('error'))
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    const fallback = document.body.querySelector('.ycloud-preview-fallback')!
    expect(fallback.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(fallback.querySelector('a')).toBeNull()
    expect(document.body.querySelector('.ycloud-preview-head a')).toBeNull()
  })

  it('shows the same quota error for a denied text document', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'traffic_exhausted' } }), { status: 429 })))
    showFile('notes.txt')
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(document.body.querySelector('.ycloud-preview-fallback')?.textContent).toContain('下载流量已用尽或剩余流量不足')
    expect(document.body.querySelector('.ycloud-preview-head a')).toBeNull()
  })

  it('uses compact icon-only download and close actions for documents', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('hello', { status: 200 })))
    showFile('notes.txt')
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    const dialog = document.body.querySelector('.ycloud-preview-dialog.document')!
    const download = dialog.querySelector<HTMLAnchorElement>('.ycloud-preview-head a')!
    const close = dialog.querySelector<HTMLButtonElement>('.ycloud-preview-head button[aria-label="关闭"]')!
    expect(download.getAttribute('aria-label')).toBe('下载')
    expect(download.textContent?.trim()).toBe('')
    expect(download.querySelector('svg')).not.toBeNull()
    expect(close.querySelector('svg')).not.toBeNull()
    expect(close.querySelector('.app-icon')).toBeNull()
  })

  it('checks PDF quota before loading the embedded viewer', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    showFile('report.pdf')
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    expect(document.body.querySelector('.ycloud-preview-dialog iframe')).toBeNull()
    expect(document.body.querySelector('.ycloud-preview-fallback')?.textContent).toContain('下载流量已用尽或剩余流量不足')
  })

  it('renders HTML in a sandboxed document view and can show its source', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('<h1>Hello</h1>', { status: 200 })))
    showFile('page.html')
    await new Promise(resolve => setTimeout(resolve, 0))
    await nextTick()
    const frame = document.body.querySelector<HTMLIFrameElement>('.ycloud-preview-dialog.document iframe')
    expect(frame?.getAttribute('sandbox')).toBe('')
    expect(frame?.getAttribute('srcdoc')).toContain("default-src 'none'")
    expect(frame?.getAttribute('srcdoc')).toContain('<h1>Hello</h1>')
    document.body.querySelectorAll<HTMLButtonElement>('.ycloud-document-modes button')[1]!.click()
    await nextTick()
    expect(document.body.querySelector('.ycloud-preview-dialog.document pre')?.textContent).toBe('<h1>Hello</h1>')
  })

  it.each(['success', 'quota'])('ignores an old text %s after replacing the dialog target', async outcome => {
    let resolve!: (response: Response) => void
    const old = new Promise<Response>(accept => { resolve = accept })
    const fetchMock = vi.fn().mockReturnValueOnce(old).mockResolvedValueOnce(new Response('new content'))
    vi.stubGlobal('fetch', fetchMock)
    const { entry } = showFile('old.txt')
    entry.path = 'new.txt'
    entry.name = 'new.txt'
    await new Promise(accept => setTimeout(accept, 0))
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    expect(fetchMock.mock.calls[1]?.[0]).toContain('new.txt')
    resolve(new Response('old content', { status: outcome === 'quota' ? 429 : 200 }))
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('pre')?.textContent).toBe('new content')
    expect(document.body.querySelector('.ycloud-preview-fallback')).toBeNull()
    expect(document.body.querySelector('.ycloud-preview-head a')).not.toBeNull()
  })

  it('cancels an unfinished text request when the dialog closes', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const { app } = showFile('notes.txt')
    app.unmount()
    mountedApps.splice(mountedApps.indexOf(app), 1)
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    await nextTick()
    expect(document.body.querySelector('.ycloud-preview-dialog')).toBeNull()
  })

  it('shows only source for oversized HTML even when the endpoint ignores Range', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('a'.repeat(2 * 1024 * 1024 + 1))))
    showFile('large.html')
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('iframe')).toBeNull()
    expect(document.body.querySelector('.ycloud-document-modes')).toBeNull()
    expect(document.body.querySelector('pre')?.textContent).toBe(`${'a'.repeat(2 * 1024 * 1024)}\n\n[预览已截断，仅显示前 2 MiB]`)
  })

  it('deduplicates consecutive document download clicks', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    showFile('clip.mp4')
    const link = document.body.querySelector<HTMLAnchorElement>('.ycloud-preview-head a')!
    link.click()
    link.click()
    expect(fetchMock).toHaveBeenCalledOnce()
  })

  it.each([200, 403])('drops an old download check (%s) after replacing the file', async status => {
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(accept => { resolve = accept }))
    vi.stubGlobal('fetch', fetchMock)
    const navigate = vi.spyOn(window.location, 'href', 'set').mockImplementation(() => {})
    const { entry } = showFile('old.mp4')
    document.body.querySelector<HTMLAnchorElement>('.ycloud-preview-head a')!.click()
    entry.path = 'new.mp4'
    entry.name = 'new.mp4'
    await nextTick()
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
    resolve(new Response(null, { status }))
    await new Promise(accept => setTimeout(accept, 0))
    expect(navigate).not.toHaveBeenCalled()
    expect(document.body.querySelector('.ycloud-preview-error')).toBeNull()
  })

  it('cancels PDF admission when the dialog closes', async () => {
    const fetchMock = vi.fn().mockImplementation(() => new Promise(() => {}))
    vi.stubGlobal('fetch', fetchMock)
    const { app } = showFile('report.pdf')
    app.unmount()
    mountedApps.splice(mountedApps.indexOf(app), 1)
    expect(fetchMock.mock.calls[0]?.[1].signal.aborted).toBe(true)
  })

  it('checks a replacement PDF rather than applying the old quota response', async () => {
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn().mockReturnValueOnce(new Promise<Response>(accept => { resolve = accept })).mockResolvedValueOnce(new Response(null))
    vi.stubGlobal('fetch', fetchMock)
    const { entry } = showFile('old.pdf')
    entry.path = 'new.pdf'
    entry.name = 'new.pdf'
    await new Promise(accept => setTimeout(accept, 0))
    expect(fetchMock).toHaveBeenCalledTimes(2)
    resolve(new Response(null, { status: 429 }))
    await new Promise(accept => setTimeout(accept, 0))
    await nextTick()
    expect(document.body.querySelector('iframe')?.getAttribute('src')).toContain('new.pdf')
    expect(document.body.querySelector('.ycloud-preview-fallback')).toBeNull()
  })
})

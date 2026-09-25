/* eslint-disable vue/one-component-per-file -- createApp receives prop objects in this component test. */
import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import PreviewView from './PreviewView.vue'

afterEach(() => {
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
      createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
    await nextTick()
    expect(host.querySelector('iframe')).toBeNull()
    expect(host.querySelector('.preview-message a')?.textContent).toBe('下载文件')
  })

  it('shows a PDF quota error before loading the embedded viewer', async () => {
    window.history.replaceState(null, '', '/preview?path=%2Freport.pdf')
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 429 })))
    const host = document.createElement('div')
    document.body.append(host)
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
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
    createApp(PreviewView, { theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() } }).mount(host)
    await nextTick()
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(host.querySelector('pre')?.textContent).toContain('[预览已截断，仅显示前 2 MiB]')
  })
})

/* eslint-disable vue/one-component-per-file -- createApp receives prop objects in this component test. */
import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import PreviewView from './PreviewView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
  window.history.replaceState(null, '', '/')
})

describe('PreviewView', () => {
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

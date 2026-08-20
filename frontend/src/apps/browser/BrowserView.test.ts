import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountBrowser(host: HTMLElement) {
  const app = createApp(BrowserView, {
    theme: { current: ref<'light' | 'dark'>('light'), toggle: vi.fn() },
  })
  app.mount(host)
  return app
}

function listResponse(names: string[]) {
  return {
    current_path: '',
    parent_path: null,
    entries: names.map((name, index) => ({
      name,
      path: name,
      is_dir: false,
      size: index + 1,
      modified: '',
      mime: 'text/plain',
      icon: 'code',
      locked: false,
    })),
    truncated: false,
    can_write: true,
    max_upload_bytes: 1024,
    max_archive_bytes: 1024,
    max_archive_entries: 100,
  }
}

function domRect(left: number, top: number, right: number, bottom: number): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    right,
    bottom,
    width: right - left,
    height: bottom - top,
    toJSON: () => ({}),
  }
}

describe('BrowserView', () => {
  it('renders a directory response without trusting HTML from file names', async () => {
    const response = {
      current_path: '',
      parent_path: null,
      entries: [{
        name: '<img src=x onerror=alert(1)>.txt',
        path: '<img src=x onerror=alert(1)>.txt',
        is_dir: false,
        size: 12,
        modified: '2026-08-20 12:00',
        mime: 'text/plain',
        icon: 'code',
        locked: false,
      }],
      truncated: false,
      can_write: false,
      max_upload_bytes: 1024,
      max_archive_bytes: 1024,
      max_archive_entries: 100,
    }
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(response), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.textContent).toContain('<img src=x onerror=alert(1)>.txt')
    expect(host.textContent).toContain('12 B')
    expect(host.querySelector('img')).toBeNull()
    app.unmount()
  })

  it('opens the mobile action panel when the header selects all entries', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      current_path: '',
      parent_path: null,
      entries: [{
        name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false,
      }],
      truncated: false,
      can_write: true,
      max_upload_bytes: 1024,
      max_archive_bytes: 1024,
      max_archive_entries: 100,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const selectAll = host.querySelector('.file-head .select-box') as HTMLButtonElement
    selectAll.click()
    await nextTick()

    expect(host.querySelector('.context-menu')).not.toBeNull()
    expect(host.textContent).toContain('已选择 1 项')
    expect(host.textContent).toContain('下载')
    app.unmount()
  })

  it('selects only intersecting rows without rendering a drag rectangle', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: false }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse(['one.txt', 'two.txt'])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const panel = host.querySelector('.file-panel') as HTMLElement
    const rows = [...host.querySelectorAll<HTMLElement>('.file-row')]
    vi.spyOn(panel, 'getBoundingClientRect').mockReturnValue(domRect(0, 40, 400, 180))
    vi.spyOn(rows[0]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 70, 380, 105))
    vi.spyOn(rows[1]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 110, 380, 145))

    panel.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, clientX: 30, clientY: 60 }))
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: 360, clientY: 95 }))
    await nextTick()

    expect(host.querySelector('.drag-selection-box')).toBeNull()
    expect(rows[0]!.classList.contains('selected')).toBe(true)
    expect(rows[1]!.classList.contains('selected')).toBe(false)

    window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, clientX: 360, clientY: 95 }))
    rows[0]!.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    await nextTick()
    expect(host.querySelector('.drag-selection-box')).toBeNull()
    expect(rows[0]!.classList.contains('selected')).toBe(true)
    app.unmount()
  })

  it('does not enable drag selection on mobile layouts', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse(['one.txt'])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const panel = host.querySelector('.file-panel') as HTMLElement
    panel.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, clientX: 20, clientY: 20 }))
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: 200, clientY: 100 }))
    window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }))
    await nextTick()

    expect(host.querySelector('.drag-selection-box')).toBeNull()
    expect(host.querySelector('.file-row.selected')).toBeNull()
    app.unmount()
  })

  it('removes intersecting selected rows when dragging upward', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: false }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse(['one.txt', 'two.txt'])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const panel = host.querySelector('.file-panel') as HTMLElement
    const rows = [...host.querySelectorAll<HTMLElement>('.file-row')]
    rows[0]!.click()
    rows[1]!.click()
    await nextTick()
    expect(host.querySelectorAll('.file-row.selected')).toHaveLength(2)
    vi.spyOn(panel, 'getBoundingClientRect').mockReturnValue(domRect(0, 40, 400, 180))
    vi.spyOn(rows[0]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 70, 380, 105))
    vi.spyOn(rows[1]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 110, 380, 145))

    panel.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, clientX: 30, clientY: 145 }))
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: 360, clientY: 75 }))
    window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, clientX: 360, clientY: 75 }))
    await nextTick()

    expect(host.querySelector('.drag-selection-box')).toBeNull()
    expect(host.querySelectorAll('.file-row.selected')).toHaveLength(0)
    app.unmount()
  })

  it('blocks external file drops without starting an upload', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse([])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await nextTick()
    const transfer = { types: ['Files'], dropEffect: 'copy' }
    const drop = new Event('drop', { bubbles: true, cancelable: true })
    Object.defineProperty(drop, 'dataTransfer', { value: transfer })

    expect(window.dispatchEvent(drop)).toBe(false)
    expect(drop.defaultPrevented).toBe(true)
    expect(transfer.dropEffect).toBe('none')
    app.unmount()
  })
})

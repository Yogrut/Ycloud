import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'
import BrowserContextMenu from './BrowserContextMenu.vue'

const file: FileEntry = {
  name: 'test.txt',
  path: 'test.txt',
  is_dir: false,
  size: 12,
  modified: '2026-08-20 12:00',
  mime: 'text/plain',
  icon: 'code',
  locked: false,
}

const mountedApps: ReturnType<typeof createApp>[] = []
const capabilities: BrowserCapabilities = {
  download: true, upload: true, create_directory: true,
  rename: true, move_items: true, copy: true, delete: true,
}

afterEach(() => {
  mountedApps.splice(0).forEach(app => app.unmount())
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

async function mountMenu(props: Record<string, unknown>): Promise<HTMLElement> {
  vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: false }))
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(BrowserContextMenu, props)
  mountedApps.push(app)
  app.mount(host)
  await nextTick()
  return host
}

describe('BrowserContextMenu', () => {
  it.each([
    [320, 480, 315, 475, 144, 252],
    [1024, 768, 1020, 765, 848, 540],
    [320, 480, -10, -10, 8, 8],
  ])('keeps the menu inside a %i x %i viewport near every edge', async (width, height, x, y, left, top) => {
    vi.stubGlobal('innerWidth', width)
    vi.stubGlobal('innerHeight', height)
    vi.stubGlobal('visualViewport', null)
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ width: 168, height: 220 } as DOMRect)
    const host = await mountMenu({ entry: file, paths: [file.path], capabilities, x, y })
    await nextTick()
    const menu = host.querySelector<HTMLElement>('.context-menu')!
    expect(menu.style.left).toBe(`${left}px`)
    expect(menu.style.top).toBe(`${top}px`)
    expect(menu.style.maxHeight).toBe(`${height - 16}px`)
  })

  it('fits within a shifted visual viewport and reclamps after it resizes', async () => {
    const viewport = Object.assign(new EventTarget(), { offsetLeft: 30, offsetTop: 50, width: 320, height: 420 })
    vi.stubGlobal('visualViewport', viewport)
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      return { width: Math.min(168, Number.parseFloat(this.style.maxWidth)), height: Math.min(220, Number.parseFloat(this.style.maxHeight)) } as DOMRect
    })
    const host = await mountMenu({ entry: file, paths: [file.path], capabilities, x: 1000, y: 1000 })
    await nextTick()
    const menu = host.querySelector<HTMLElement>('.context-menu')!
    expect(menu.style.left).toBe('174px')
    expect(menu.style.top).toBe('242px')
    expect(menu.style.maxHeight).toBe('404px')
    viewport.height = 140
    viewport.dispatchEvent(new Event('resize'))
    await nextTick()
    await nextTick()
    expect(menu.style.top).toBe('58px')
    expect(menu.style.maxHeight).toBe('124px')
  })

  it('keeps upload and new-folder actions visible when no permissions are granted', async () => {
    const action = vi.fn()
    const host = await mountMenu({ entry: file, paths: ['test.txt'], x: 20, y: 20, onAction: action,
      capabilities: { download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false },
    })
    const buttons = [...host.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')]
    expect(buttons.map(button => button.textContent)).toEqual(['上传', '新建文件夹'])
    buttons[0]!.click()
    buttons[1]!.click()
    expect(action.mock.calls).toEqual([['upload'], ['mkdir']])
  })
  it('keeps single-file preview on double-click and exposes download in the menu', async () => {
    const host = await mountMenu({ entry: file, paths: ['test.txt'], capabilities, x: 20, y: 20 })

    expect(host.textContent).toContain('下载')
    expect(host.textContent).not.toContain('预览')
    expect(host.textContent).toContain('重命名')
  })

  it('shows the selected count for destructive multi-selection actions', async () => {
    const host = await mountMenu({ entry: null, paths: ['one.txt', 'two.txt'], capabilities, x: 20, y: 20 })

    expect(host.textContent).toContain('已选择 2 项')
    expect(host.textContent).toContain('删除 (2)')
    expect(host.textContent).toContain('打包下载')
  })
})

import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { FileEntry } from '../../shared/api/browser'
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

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

async function mountMenu(props: Record<string, unknown>): Promise<HTMLElement> {
  vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: false }))
  const host = document.createElement('div')
  document.body.append(host)
  createApp(BrowserContextMenu, props).mount(host)
  await nextTick()
  return host
}

describe('BrowserContextMenu', () => {
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
    const host = await mountMenu({ entry: file, paths: ['test.txt'], canWrite: true, x: 20, y: 20 })

    expect(host.textContent).toContain('下载')
    expect(host.textContent).not.toContain('预览')
    expect(host.textContent).toContain('重命名')
  })

  it('shows the selected count for destructive multi-selection actions', async () => {
    const host = await mountMenu({ entry: null, paths: ['one.txt', 'two.txt'], canWrite: true, x: 20, y: 20 })

    expect(host.textContent).toContain('已选择 2 项')
    expect(host.textContent).toContain('删除 (2)')
    expect(host.textContent).toContain('打包下载')
  })
})

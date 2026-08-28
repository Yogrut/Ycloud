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

async function chooseOption(host: HTMLElement, selector: string, label: string): Promise<void> {
  host.querySelector<HTMLButtonElement>(`${selector} .app-select-trigger`)?.click()
  await nextTick()
  const option = [...host.querySelectorAll<HTMLButtonElement>(`${selector} .app-select-option`)]
    .find(item => item.textContent?.trim() === label)
  if (!option) throw new Error(`select option missing: ${label}`)
  option.click()
}

function listResponse(names: string[]) {
  return {
    storage_id: 'primary',
    storages: [],
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
    page_start: names.length ? 1 : 0,
    page_size: 20,
    next_cursor: null,
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
  it('places a compact lock indicator after a locked folder name', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      ...listResponse([]),
      entries: [{
        name: 'private', path: 'private', is_dir: true, size: 0, modified: '', mime: '', icon: 'folder', locked: true,
      }],
      page_start: 1,
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    const name = host.querySelector('.file-name')!
    const label = name.querySelector('.file-label')!
    const lock = name.querySelector('.file-lock-indicator')!
    expect(label.nextElementSibling).toBe(lock)
    expect(name.querySelector('.file-icon .file-lock-indicator')).toBeNull()
    expect(lock.querySelector('[data-icon="lock"]')).not.toBeNull()
    app.unmount()
  })

  it('renders a directory response without trusting HTML from file names', async () => {
    const response = {
      storage_id: 'primary',
      storages: [],
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
      page_start: 1,
      page_size: 20,
      next_cursor: null,
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

  it('keeps page actions inside the file panel and uses only the row context menu', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse(['notes.txt'])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.browser-chrome .top-search')).toBeNull()
    expect(host.querySelector('.file-toolbar .top-search')).not.toBeNull()
    expect(host.querySelector('.file-toolbar')?.textContent).toContain('新建文件夹')
    expect(host.querySelector('.file-toolbar')?.textContent).toContain('上传文件')
    expect(host.querySelector('.file-row-menu')).toBeNull()
    const row = host.querySelector('.file-row') as HTMLElement
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, button: 2, clientX: 40, clientY: 40 }))
    await nextTick()
    expect(host.querySelector('.context-menu')).not.toBeNull()
    expect(host.querySelector('.file-row.selected')).not.toBeNull()
    app.unmount()
  })

  it('lets an administrator switch storage without mixing the previous path or selection', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({
        ...listResponse(['one.txt']),
        current_path: 'games',
        storages: [
          { id: 'primary', name: 'Local', requires_login: false },
          { id: 'rustfs', name: 'RustFS', requires_login: false },
        ],
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        ...listResponse(['remote.txt']),
        storage_id: 'rustfs',
        storages: [
          { id: 'primary', name: 'Local', requires_login: false },
          { id: 'rustfs', name: 'RustFS', requires_login: false },
        ],
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    const firstRow = host.querySelector('.file-row') as HTMLElement
    firstRow.click()
    await nextTick()
    expect(firstRow.classList.contains('selected')).toBe(true)

    expect(host.querySelector('.browser-chrome .storage-switcher')).not.toBeNull()
    expect(host.querySelector('.breadcrumb .storage-switcher')).toBeNull()
    expect(host.querySelector('.browser-chrome .storage-switcher svg')).toBeNull()
    await chooseOption(host, '.storage-switcher', 'RustFS')
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/files?storage_id=rustfs&limit=20&sort=name&direction=asc', { credentials: 'same-origin' })
    expect(host.textContent).toContain('remote.txt')
    expect(host.textContent).not.toContain('one.txt')
    expect(host.querySelector('.file-row.selected')).toBeNull()
    const home = host.querySelector<HTMLButtonElement>('.home-crumb[aria-current="location"]')
    expect(home?.getAttribute('aria-label')).toBe('首页')
    const homeIcon = home?.querySelector('[data-icon="home"][data-weight="duotone"]')
    expect(homeIcon?.tagName.toLowerCase()).toBe('svg')
    expect(home?.textContent?.trim()).toBe('')
    app.unmount()
  })

  it('asks for an authorized account before opening a restricted storage', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      ...listResponse(['public.txt']),
      storages: [
        { id: 'primary', name: 'Public', requires_login: false },
        { id: 'private', name: 'Private', requires_login: true },
      ],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)
    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    await chooseOption(host, '.storage-switcher', 'Private（需登录）')
    await nextTick()

    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(host.querySelector('.storage-switcher .app-select-trigger')?.textContent).toContain('Public')
    expect(host.querySelector<HTMLFormElement>('.overlay.active form')?.textContent).toContain('账号登录')
    app.unmount()
  })

  it('uses server cursors for the next and previous directory pages', async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({
        ...listResponse(['first.txt']),
        page_start: 1,
        next_cursor: 'MjA',
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        ...listResponse(['second.txt']),
        page_start: 21,
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        ...listResponse(['first.txt']),
        page_start: 1,
        next_cursor: 'MjA',
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetchMock)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.browser-chrome.glass')).not.toBeNull()
    expect(host.textContent).not.toContain('显示第')
    expect(host.querySelector('.page-size-select [data-icon]')).toBeNull()
    expect(host.querySelectorAll('.page-chevron')).toHaveLength(2)
    const arrows = host.querySelectorAll<HTMLButtonElement>('.page-arrow')
    arrows[1]!.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/files?storage_id=primary&limit=20&cursor=MjA&sort=name&direction=asc', { credentials: 'same-origin' })
    expect(host.textContent).toContain('second.txt')
    expect(host.querySelector('.current-page')?.textContent).toBe('2')

    host.querySelector<HTMLButtonElement>('.page-arrow')!.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(fetchMock).toHaveBeenNthCalledWith(3, '/api/files?storage_id=primary&limit=20&sort=name&direction=asc', { credentials: 'same-origin' })
    expect(host.textContent).toContain('first.txt')
    app.unmount()
  })

  it('changes page size through the project dropdown on desktop', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: false }))
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse(['one.txt'])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.page-size-select select')).toBeNull()
    await chooseOption(host, '.page-size-select', '10')
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/files?storage_id=primary&limit=10&sort=name&direction=asc', { credentials: 'same-origin' })
    app.unmount()
  })

  it('opens the mobile action panel when the header selects all entries', async () => {
    vi.stubGlobal('matchMedia', vi.fn().mockReturnValue({ matches: true }))
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      storage_id: 'primary',
      storages: [],
      current_path: '',
      parent_path: null,
      entries: [{
        name: 'one.txt', path: 'one.txt', is_dir: false, size: 1, modified: '', mime: 'text/plain', icon: 'code', locked: false,
      }],
      page_start: 1,
      page_size: 20,
      next_cursor: null,
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
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: 360, clientY: 95 }))
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

  it('starts desktop drag selection from a row checkbox without toggling text selection', async () => {
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
    const checkbox = rows[0]!.querySelector('.select-box') as HTMLButtonElement
    vi.spyOn(panel, 'getBoundingClientRect').mockReturnValue(domRect(0, 40, 400, 180))
    vi.spyOn(rows[0]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 70, 380, 105))
    vi.spyOn(rows[1]!, 'getBoundingClientRect').mockReturnValue(domRect(20, 110, 380, 145))

    const down = new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0, buttons: 1, clientX: 30, clientY: 80 })
    checkbox.dispatchEvent(down)
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, cancelable: true, buttons: 1, clientX: 360, clientY: 130 }))
    await nextTick()

    expect(down.defaultPrevented).toBe(true)
    expect(host.querySelectorAll('.file-row.selected')).toHaveLength(2)
    window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: 360, clientY: 130 }))
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
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: 200, clientY: 100 }))
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
    window.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: 360, clientY: 75 }))
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

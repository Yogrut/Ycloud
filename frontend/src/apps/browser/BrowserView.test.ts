import { createApp, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import BrowserView from './BrowserView.vue'

afterEach(() => {
  vi.unstubAllGlobals()
  document.body.replaceChildren()
})

function mountBrowser(host: HTMLElement) {
  const originalFetch = globalThis.fetch
  vi.stubGlobal('fetch', (input: RequestInfo | URL, init?: RequestInit) => input === '/api/me'
    ? Promise.resolve(new Response(JSON.stringify({ logged_in: false, is_admin: false, username: null }), { status: 200 }))
    : originalFetch(input, init))
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
    max_upload_batch_bytes: 4096,
    max_upload_batch_entries: 100,
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
    expect(host.querySelector('.browser-shell')).not.toBeNull()
    expect(host.querySelector('.file-list-body')).not.toBeNull()
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
    expect(host.querySelectorAll('.file-toolbar-actions .btn')).toHaveLength(2)
    host.querySelector<HTMLButtonElement>('.file-toolbar-actions .btn')?.click()
    await nextTick()
    expect(document.querySelector('.app-toast.error')?.textContent).toBe('无权限')
    expect(document.querySelector('.user-account-modal')).toBeNull()
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
    expect(host.querySelector('.file-toolbar')?.textContent).toContain('新建')
    expect(host.querySelectorAll('.file-toolbar-actions .btn')).toHaveLength(2)
    const uploadButton = [...host.querySelectorAll<HTMLButtonElement>('.file-toolbar-actions .btn')]
      .find(button => button.textContent?.trim() === '上传')
    expect(uploadButton).toBeDefined()
    uploadButton?.click()
    await nextTick()
    expect(host.querySelector('.upload-queue-modal')?.textContent).toContain('选择文件')
    expect(host.querySelector('.upload-queue-modal')?.textContent).toContain('选择文件夹')
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
    expect(document.querySelector('.user-account-modal')?.textContent).toContain('用户登录')
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

  it('retries a failed upload with its existing batch ticket', async () => {
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      const body = url.includes('/api/upload/prepare') ? { ticket: 'batch-1' } : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
    })
    vi.stubGlobal('fetch', fetchMock)
    const statuses = [400, 204]
    class MockXMLHttpRequest {
      status = 0
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      private progress?: (event: ProgressEvent) => void
      upload = { addEventListener: (_type: string, listener: (event: ProgressEvent) => void) => { this.progress = listener } }
      open(): void {}
      setRequestHeader(): void {}
      getResponseHeader(): null { return null }
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(file: File): void {
        this.progress?.({ lengthComputable: true, loaded: file.size } as ProgressEvent)
        this.status = statuses.shift() ?? 204
        this.responseText = this.status >= 400 ? JSON.stringify({ message: 'temporary failure' }) : ''
        this.listeners.get('load')?.()
      }
    }
    vi.stubGlobal('XMLHttpRequest', MockXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['hello'], 'retry.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelector('.upload-queue-modal')?.textContent).toContain('temporary failure')

    const retry = host.querySelector<HTMLButtonElement>('button[aria-label="重试该文件"]')
    retry?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.upload-task.is-succeeded')?.textContent).toContain('retry.txt')
    expect(host.querySelector('.upload-task.is-succeeded')?.textContent).toContain('成功')
    expect(fetchMock.mock.calls.filter(([input]) => String(input).includes('/api/upload/prepare'))).toHaveLength(1)
    app.unmount()
  })

  it('pauses queued files, resumes them, terminates the active upload, and clears only task records', async () => {
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      const body = url.includes('/api/upload/prepare')
        ? { ticket: 'batch-controls' }
        : url.includes('/api/upload/status')
          ? { ticket: 'batch-controls', items: [{ path: 'two.txt', size: 3, status: 'failed' }] }
          : url.includes('/api/upload/cancel')
            ? { success: true }
            : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
    })
    vi.stubGlobal('fetch', fetchMock)
    class ControlledXMLHttpRequest {
      static instances: ControlledXMLHttpRequest[] = []
      status = 0
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      private progress?: (event: ProgressEvent) => void
      upload = { addEventListener: (_type: string, listener: (event: ProgressEvent) => void) => { this.progress = listener } }
      constructor() { ControlledXMLHttpRequest.instances.push(this) }
      open(): void {}
      setRequestHeader(): void {}
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void { this.progress?.({ lengthComputable: true, loaded: 1 } as ProgressEvent) }
      abort(): void { this.listeners.get('abort')?.() }
      complete(status = 204): void { this.status = status; this.listeners.get('load')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', ControlledXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['one'], 'one.txt'), new File(['two'], 'two.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(ControlledXMLHttpRequest.instances).toHaveLength(1)
    host.querySelector<HTMLButtonElement>('.upload-task.is-queued button[aria-label="暂停该文件"]')?.click()
    await nextTick()
    expect(host.querySelectorAll('.upload-task.is-paused')).toHaveLength(1)

    ControlledXMLHttpRequest.instances[0]?.complete()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelectorAll('.upload-task.is-succeeded')).toHaveLength(1)
    expect(host.querySelectorAll('.upload-task.is-paused')).toHaveLength(1)

    host.querySelector<HTMLButtonElement>('.upload-task.is-paused button[aria-label="继续该文件"]')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(ControlledXMLHttpRequest.instances).toHaveLength(2)
    host.querySelector<HTMLButtonElement>('.upload-task.is-uploading button[aria-label="终止该文件"]')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelectorAll('.upload-task.is-cancelled')).toHaveLength(1)
    const cancelCall = fetchMock.mock.calls.find(([input]) => String(input).includes('/api/upload/cancel'))
    expect(cancelCall?.[1]).toEqual(expect.objectContaining({
      body: JSON.stringify({ ticket: 'batch-controls', paths: ['two.txt'] }),
    }))

    const filters = [...host.querySelectorAll<HTMLButtonElement>('.upload-filter-tab')]
    filters.find(button => button.textContent?.includes('成功'))?.click()
    await nextTick()
    host.querySelector<HTMLButtonElement>('button[aria-label="删除当前筛选任务记录"]')?.click()
    await nextTick()
    expect(host.querySelectorAll('.upload-task')).toHaveLength(0)

    filters.find(button => button.textContent?.includes('全部'))?.click()
    await nextTick()
    expect(host.querySelectorAll('.upload-task.is-cancelled')).toHaveLength(1)
    host.querySelector<HTMLButtonElement>('button[aria-label="删除当前筛选任务记录"]')?.click()
    await nextTick()
    expect(host.querySelectorAll('.upload-task')).toHaveLength(0)
    expect(host.querySelector('.upload-empty-state')?.textContent).toContain('选择文件或拖拽')
    app.unmount()
  })

  it('reconciles a lost upload response from the server batch result', async () => {
    let statusQueries = 0
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      const body = url.includes('/api/upload/prepare')
        ? { ticket: 'batch-reconcile' }
        : url.includes('/api/upload/status')
          ? {
              ticket: 'batch-reconcile',
              items: [{
                path: 'reconciled.txt',
                size: 4,
                status: statusQueries++ === 0 ? 'in_progress' : 'complete',
              }],
            }
          : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
    })
    vi.stubGlobal('fetch', fetchMock)
    class LostResponseXMLHttpRequest {
      status = 0
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      upload = { addEventListener: () => undefined }
      open(): void {}
      setRequestHeader(): void {}
      getResponseHeader(): null { return null }
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void { this.listeners.get('error')?.() }
      abort(): void { this.listeners.get('abort')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', LostResponseXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['data'], 'reconciled.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.upload-task.is-succeeded')?.textContent).toContain('reconciled.txt')
    expect(fetchMock.mock.calls.filter(([request]) => String(request).includes('/api/upload/status?batch=batch-reconcile'))).toHaveLength(2)
    expect(host.querySelector('[aria-label="重试该文件"]')).toBeNull()
    app.unmount()
  })

  it('pauses an active upload only after the server confirms it was not committed', async () => {
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      const body = url.includes('/api/upload/prepare')
        ? { ticket: 'pause-ticket' }
        : url.includes('/api/upload/status')
          ? { ticket: 'pause-ticket', items: [{ path: 'pause.txt', size: 4, status: 'failed' }] }
          : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      }))
    })
    vi.stubGlobal('fetch', fetchMock)
    class PausableXMLHttpRequest {
      static instances: PausableXMLHttpRequest[] = []
      status = 204
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      upload = { addEventListener: () => undefined }
      constructor() { PausableXMLHttpRequest.instances.push(this) }
      open(): void {}
      setRequestHeader(): void {}
      getResponseHeader(): null { return null }
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void {
        if (PausableXMLHttpRequest.instances.length > 1) this.listeners.get('load')?.()
      }
      abort(): void { this.listeners.get('abort')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', PausableXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['data'], 'pause.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    host.querySelector<HTMLButtonElement>('.upload-task.is-uploading button[aria-label="暂停该文件"]')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(host.querySelector('.upload-task.is-paused')?.textContent).toContain('已暂停')
    expect(fetchMock.mock.calls.some(([request]) => String(request).includes('/api/upload/status?batch=pause-ticket'))).toBe(true)

    host.querySelector<HTMLButtonElement>('.upload-task.is-paused button[aria-label="继续该文件"]')?.click()
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    expect(PausableXMLHttpRequest.instances).toHaveLength(2)
    expect(host.querySelector('.upload-task.is-succeeded')?.textContent).toContain('pause.txt')
    app.unmount()
  })

  it('accepts file drops on the file panel and keeps rejected files as removable errors', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(listResponse([])), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })))

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const panel = host.querySelector('.file-panel') as HTMLElement
    const transfer = {
      types: ['Files'],
      items: [],
      files: [new File([new Uint8Array(2048)], 'too-large.bin')],
      dropEffect: 'none',
    }
    const enter = new Event('dragenter', { bubbles: true, cancelable: true })
    Object.defineProperty(enter, 'dataTransfer', { value: transfer })
    panel.dispatchEvent(enter)
    await nextTick()
    expect(host.querySelector('.upload-drop-overlay')).not.toBeNull()

    const drop = new Event('drop', { bubbles: true, cancelable: true })
    Object.defineProperty(drop, 'dataTransfer', { value: transfer })
    panel.dispatchEvent(drop)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.upload-drop-overlay')).toBeNull()
    expect(host.querySelector('.upload-queue-modal')?.textContent).toContain('too-large.bin')
    expect(host.querySelector('button[aria-label="重试该文件"]')).not.toBeNull()
    expect(host.querySelector('button[aria-label="删除失败记录"]')).not.toBeNull()
    app.unmount()
  })

  it('keeps an upload bound to the storage and directory where it was queued', async () => {
    let resolvePrepare!: (response: Response) => void
    const prepareResponse = new Promise<Response>(resolve => { resolvePrepare = resolve })
    const storages = [
      { id: 'primary', name: 'Local', requires_login: false },
      { id: 'archive', name: 'Archive', requires_login: false },
    ]
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes('/api/upload/prepare')) return prepareResponse
      const storageId = new URL(url, 'http://localhost').searchParams.get('storage_id')
      return Promise.resolve(new Response(JSON.stringify({
        ...listResponse([]),
        storage_id: storageId ?? 'primary',
        current_path: storageId === 'archive' ? '' : 'incoming',
        storages,
      }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    })
    vi.stubGlobal('fetch', fetchMock)

    class RecordingXMLHttpRequest {
      static urls: string[] = []
      status = 204
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      upload = { addEventListener: () => undefined }
      open(_method: string, url: string): void { RecordingXMLHttpRequest.urls.push(url) }
      setRequestHeader(): void {}
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void { this.listeners.get('load')?.() }
      abort(): void { this.listeners.get('abort')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', RecordingXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['data'], 'frozen.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await nextTick()
    expect(host.querySelector('.upload-task.is-preparing')).not.toBeNull()

    await chooseOption(host, '.storage-switcher', 'Archive')
    await new Promise(resolve => window.setTimeout(resolve, 0))
    resolvePrepare(new Response(JSON.stringify({ ticket: 'frozen-ticket' }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(RecordingXMLHttpRequest.urls).toHaveLength(1)
    const uploadUrl = new URL(RecordingXMLHttpRequest.urls[0]!, 'http://localhost')
    expect(uploadUrl.searchParams.get('storage_id')).toBe('primary')
    expect(uploadUrl.searchParams.get('path')).toBe('/incoming/frozen.txt')
    app.unmount()
  })

  it('cancels a server batch when its only task is terminated during preparation', async () => {
    let resolvePrepare!: (response: Response) => void
    const prepareResponse = new Promise<Response>(resolve => { resolvePrepare = resolve })
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes('/api/upload/prepare')) return prepareResponse
      const body = url.includes('/api/upload/cancel') ? { success: true } : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    })
    vi.stubGlobal('fetch', fetchMock)
    const xhr = vi.fn()
    vi.stubGlobal('XMLHttpRequest', xhr)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File(['data'], 'cancelled.txt')] })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await nextTick()

    host.querySelector<HTMLButtonElement>('.upload-task.is-preparing button[aria-label="终止该文件"]')?.click()
    resolvePrepare(new Response(JSON.stringify({ ticket: 'cancelled-ticket' }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelector('.upload-task.is-cancelled')?.textContent).toContain('cancelled.txt')
    expect(xhr).not.toHaveBeenCalled()
    const cancelCall = fetchMock.mock.calls.find(([request]) => String(request).includes('/api/upload/cancel'))
    expect(cancelCall).toBeDefined()
    expect(new URL(String(cancelCall?.[0]), 'http://localhost').searchParams.get('storage_id')).toBe('primary')
    expect(cancelCall?.[1]).toEqual(expect.objectContaining({
      body: JSON.stringify({ ticket: 'cancelled-ticket', paths: ['cancelled.txt'] }),
    }))
    app.unmount()
  })

  it('cancels one preparing item without discarding the sibling batch ticket', async () => {
    let resolvePrepare!: (response: Response) => void
    const prepareResponse = new Promise<Response>(resolve => { resolvePrepare = resolve })
    const fetchMock = vi.fn().mockImplementation((input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes('/api/upload/prepare')) return prepareResponse
      const body = url.includes('/api/upload/cancel') ? { success: true } : listResponse([])
      return Promise.resolve(new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    })
    vi.stubGlobal('fetch', fetchMock)
    class SuccessfulXMLHttpRequest {
      static urls: string[] = []
      status = 204
      responseText = ''
      withCredentials = false
      private listeners = new Map<string, () => void>()
      upload = { addEventListener: () => undefined }
      open(_method: string, url: string): void { SuccessfulXMLHttpRequest.urls.push(url) }
      setRequestHeader(): void {}
      getResponseHeader(): null { return null }
      addEventListener(type: string, listener: () => void): void { this.listeners.set(type, listener) }
      send(): void { this.listeners.get('load')?.() }
      abort(): void { this.listeners.get('abort')?.() }
    }
    vi.stubGlobal('XMLHttpRequest', SuccessfulXMLHttpRequest)

    const host = document.createElement('div')
    document.body.append(host)
    const app = mountBrowser(host)
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()
    const input = host.querySelector<HTMLInputElement>('input[type="file"]:not([webkitdirectory])')!
    Object.defineProperty(input, 'files', {
      configurable: true,
      value: [new File(['one'], 'one.txt'), new File(['two'], 'two.txt')],
    })
    input.dispatchEvent(new Event('change', { bubbles: true }))
    await nextTick()

    host.querySelector<HTMLButtonElement>('.upload-task.is-preparing button[aria-label="终止该文件"]')?.click()
    resolvePrepare(new Response(JSON.stringify({ ticket: 'shared-ticket' }), { status: 200, headers: { 'Content-Type': 'application/json' } }))
    await new Promise(resolve => window.setTimeout(resolve, 0))
    await nextTick()

    expect(host.querySelectorAll('.upload-task.is-cancelled')).toHaveLength(1)
    expect(host.querySelectorAll('.upload-task.is-succeeded')).toHaveLength(1)
    expect(SuccessfulXMLHttpRequest.urls).toHaveLength(1)
    expect(SuccessfulXMLHttpRequest.urls[0]).toContain('batch=shared-ticket')
    const cancelCall = fetchMock.mock.calls.find(([request]) => String(request).includes('/api/upload/cancel'))
    expect(cancelCall?.[1]).toEqual(expect.objectContaining({
      body: JSON.stringify({ ticket: 'shared-ticket', paths: ['one.txt'] }),
    }))
    app.unmount()
  })
})

import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App.vue'

const imports = vi.hoisted(() => ({ admin: vi.fn(), browser: vi.fn(), preview: vi.fn() }))
vi.mock('../admin/AdminView.vue', () => {
  imports.admin()
  return { default: { template: '<section data-page="admin">Admin page</section>' } }
})
vi.mock('../browser/BrowserView.vue', () => {
  imports.browser()
  return { default: { template: '<section data-page="browser">Browser page</section>' } }
})
vi.mock('../preview/PreviewView.vue', () => {
  imports.preview()
  return { default: { template: '<section data-page="preview">Preview page</section>' } }
})

afterEach(() => {
  sessionStorage.removeItem('ycloud-stay-signed-out')
  document.body.replaceChildren()
  window.history.replaceState(null, '', '/')
})

describe('lazy application routes', () => {
  it('keeps the login entry eager without importing unselected pages', () => {
    window.history.replaceState(null, '', '/')
    sessionStorage.setItem('ycloud-stay-signed-out', '1')
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(App)
    app.mount(host)
    try {
      expect(host.querySelector('.login-page')).not.toBeNull()
      expect(imports.admin).not.toHaveBeenCalled()
      expect(imports.browser).not.toHaveBeenCalled()
      expect(imports.preview).not.toHaveBeenCalled()
    } finally { app.unmount() }
  })

  it.each([
    ['/browse', 'browser'], ['/browse/', 'browser'], ['/preview', 'preview'],
    ['/admin', 'admin'], ['/admin/users/', 'admin'],
  ])('loads only the selected page for %s', async (path, page) => {
    window.history.replaceState(null, '', path)
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(App)
    app.mount(host)
    try {
      await vi.dynamicImportSettled()
      await nextTick()
      expect(host.querySelector('[data-page]')?.getAttribute('data-page')).toBe(page)
      expect(host.querySelectorAll('[data-page]')).toHaveLength(1)
      expect(host.querySelector('.login-page')).toBeNull()
    } finally { app.unmount() }
  })
})

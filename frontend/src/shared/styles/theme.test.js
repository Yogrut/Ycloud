import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const themeCss = readFileSync('src/shared/styles/theme.css', 'utf8')

describe('interactive focus styling', () => {
  it('keeps the dark login page and the space above its header on the same background', () => {
    expect(themeCss).toMatch(/:root\[data-theme="dark"\]:has\(\.login-page\)\s*\{\s*--bg: #111214;/)
    expect(themeCss).toMatch(/\.login-page\s*\{[^}]*display: flow-root;/)
  })

  it('styles disabled setting fields and places required markers beside labels', () => {
    const css = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(css).toMatch(/input:disabled[^}]+cursor: not-allowed;[^}]+box-shadow: none;/)
    expect(css).toContain('background: var(--panel-soft);')
    expect(css).toContain('input[aria-required="true"]')
    expect(css).toContain("content: '*'; position: absolute; left: -10px; top: 0; color: var(--danger); font-size: 12px;")
  })
  it('keeps inline form failures red inside modals', () => {
    expect(themeCss).toMatch(/\.admin-form-error, \.modal \.admin-form-error\s*\{[^}]*color: var\(--danger\);/)
  })

  it('keeps breadcrumb focus outlines inside the horizontal scroll clip', () => {
    expect(themeCss).toMatch(/\.breadcrumb\s*\{[^}]*overflow-x: auto;/)
    expect(themeCss).toMatch(/\.crumb:focus-visible\s*\{[^}]*outline-offset: -2px;/)
  })

  it('hides number spinners across browsers without changing numeric input types', () => {
    expect(themeCss).toMatch(/input\[type="number"\]\s*\{[^}]*-moz-appearance: textfield;[^}]*appearance: textfield;/)
    expect(themeCss).toContain('input[type="number"]::-webkit-inner-spin-button,')
    expect(themeCss).toMatch(/input\[type="number"\]::-webkit-outer-spin-button\s*\{[^}]*-webkit-appearance: none;[^}]*margin: 0;/)
  })

  it('hides pointer focus rings while retaining a themed keyboard focus indicator', () => {
    expect(themeCss).toContain(':focus:not(:focus-visible)')
    expect(themeCss).toContain(':focus-visible')
    expect(themeCss).toContain('outline: 2px solid var(--accent)')
  })

  it('defines a low-contrast neutral light hierarchy and a separate dark theme', () => {
    expect(themeCss).toContain('--bg: #f2f3f5')
    expect(themeCss).toContain('--panel: #ffffff')
    expect(themeCss).toContain('--panel-soft: #f6f7f9')
    expect(themeCss).toContain('--field: #f9fafb')
    expect(themeCss).toContain('--accent: #0874f9')
    expect(themeCss).toContain('--colored-button-text: #005fee')
    expect(themeCss).toContain('--colored-button-bg: #e5eefd')
    expect(themeCss).toContain('--colored-button-border: #7faef5')
    expect(themeCss).toMatch(/\.btn\s*\{[^}]*color: #fff;[^}]*background: #005fee;[^}]*border: 1px solid #005fee;/s)
    expect(themeCss).toMatch(/\.btn:not\(\.secondary\):not\(\.danger\):hover:not\(:disabled\)/)
    expect(themeCss).toMatch(/\.btn\.secondary\s*\{[^}]*color: var\(--text\);/)
    expect(themeCss).toMatch(/\.btn\.danger\s*\{[^}]*background: var\(--danger\);[^}]*border-color: var\(--danger\);/)
    expect(themeCss).toContain(':root[data-theme="dark"]')
    expect(themeCss).toContain('--bg: #111722')
    expect(themeCss).toContain('--panel: #171e2a')
  })

  it('keeps the desktop admin content aligned with the navigation panel', () => {
    expect(themeCss).not.toContain('gap: 18px; padding-top: 64px;')
  })

  it('preserves Ycloud navigation colours while compacting the layout', () => {
    const layoutCss = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(layoutCss).toMatch(/\.admin-nav\s*\{[^}]*background: var\(--panel\);/)
    expect(layoutCss).toMatch(/\.admin-nav-item\s*\{[^}]*border: 0;[^}]*background: transparent;[^}]*color: var\(--muted\);/)
    expect(layoutCss).toMatch(/\.admin-nav-item\.active\s*\{[^}]*color: var\(--accent\);[^}]*background: var\(--accent-soft\);/)
    expect(layoutCss).not.toContain('.admin-nav-item.active::before')
    expect(layoutCss).not.toContain('.admin-nav-item .app-icon')
  })

  it('uses the administrator login typography for every masked password input', () => {
    expect(themeCss).toMatch(/input\[type="password"\]\s*\{[^}]*font-family: var\(--font\);[^}]*font-size: 12px;[^}]*font-weight: 700;[^}]*letter-spacing: -\.18em;/s)
    expect(themeCss).toMatch(/input\[type="password"\]::placeholder\s*\{[^}]*letter-spacing: normal;/s)
    expect(themeCss.match(/input\[type="password"\]\s*\{/g)).toHaveLength(1)
  })

  it('keeps the mobile login card contained without overriding shared password tracking', () => {
    const mobile = themeCss.match(/@media \(max-width: 560px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).not.toContain('input[type="password"]')
    expect(mobile).toContain('.login-card { min-width: 0; }')
    expect(themeCss).toContain('.login-content {\n  width: min(1100px, 100%);')
    const view = readFileSync('src/apps/login/LoginView.vue', 'utf8')
    expect(view).toContain('class="h-11 rounded-full px-4 text-sm md:text-sm"')
    expect(view).toContain('class="h-11 w-full rounded-full text-sm font-semibold"')
    expect(themeCss).toContain('.login-header {\n  width: min(1400px, calc(100% - 36px));')
    expect(themeCss).toContain('.login-card [data-slot="input"] { color: var(--login-text); background: var(--login-field); border-color: transparent;')
    expect(themeCss).toContain('.login-card [data-slot="button"] { border-radius: 9999px; }')
  })

  it('scopes the Ycloud login palette and shadcn-vue components to the entry page', () => {
    expect(themeCss).toContain(':root[data-theme="dark"] .login-page {')
    expect(themeCss).toMatch(/\.login-page\s*\{[^}]*--login-bg: #f2f3f5;[^}]*--login-action: #005fee;/s)
    expect(themeCss).toMatch(/:root\[data-theme="dark"\] \.login-page\s*\{[^}]*--login-bg: #111214;[^}]*--login-panel: #1c1d20;[^}]*--login-field: #27282c;/s)
    const css = readFileSync('src/apps/login/shadcn.css', 'utf8')
    expect(css).toContain('@import "tailwindcss/utilities.css" layer(utilities) source(none);')
    expect(css).not.toContain('@import "tailwindcss/preflight.css"')
    const view = readFileSync('src/apps/login/LoginView.vue', 'utf8')
    for (const component of ['Button', 'Card', 'Input', 'Label']) {
      expect(view).toContain(`<${component}`)
    }
  })

  it('marks clearing logs as a destructive action rather than a blue secondary action', () => {
    const layoutCss = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(layoutCss).toMatch(/\.log-toolbar \.log-clear\s*\{[^}]*color: var\(--danger\);[^}]*background: var\(--panel\);/)
    expect(layoutCss).not.toContain('.log-toolbar .log-clear { color: var(--colored-button-text)')
  })

  it('keeps file-browser rounding scoped to outer surfaces and utility controls', () => {
    const layoutCss = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(layoutCss).toContain('.admin-shell .admin-header.glass,')
    expect(layoutCss).toContain('.admin-shell .admin-pane.glass,')
    expect(layoutCss).toContain('.admin-shell .dashboard-block.glass { border-radius: 12px; }')
    expect(layoutCss).toContain('.admin-shell .admin-nav-item,')
    expect(layoutCss).toContain('.browser-chrome.glass, .file-panel.glass { border-radius: 12px; }')
    expect(layoutCss).toContain('.browser-chrome .icon-btn, .file-toolbar-actions .btn { border-radius: 7px; }')
    expect(layoutCss).toMatch(/\.file-search\s*\{[^}]*background: var\(--panel-soft\);[^}]*border-radius: 8px;/)
    expect(layoutCss).not.toMatch(/\.file-(?:head|row)\s*\{[^}]*border-radius:/)
    expect(layoutCss).not.toMatch(/\.admin-nav-item\.glass|\.file-toolbar-actions \.btn\.glass/)
    expect(themeCss).toMatch(/:root\[data-theme="dark"\] body:has\(\.browser-shell\),\s*:root\[data-theme="dark"\] body:has\(\.admin-shell\),\s*:root\[data-theme="dark"\] body:has\(\.admin-login-shell\)\s*\{[^}]*--panel: #1c1d20;/)
  })

  it('does not override shared password typography in the user login card', () => {
    const userMenu = readFileSync('src/apps/browser/UserAccountMenu.vue', 'utf8')
    const inputRule = userMenu.match(/\.user-login-form \.input\s*\{([^}]*)\}/)?.[1] ?? ''
    expect(inputRule).not.toMatch(/font(?:-family|-size|-weight)?\s*:|letter-spacing\s*:/)
    expect(userMenu).toContain('.user-login-form .input:not([type="password"])')
    expect(userMenu).toContain('width: min(420px, 100%);')
    expect(userMenu).toContain('.user-login-form .user-login-submit { width: 100%; min-height: 42px;')
    expect(themeCss).toContain('.admin-login-form-pane .admin-login-submit { width: 100%; min-height: 42px;')
  })

  it('keeps the file browser compact and exposes mobile actions below search', () => {
    expect(themeCss).toContain('.file-row { min-height: 50px; cursor: default; user-select: none; }')
    expect(themeCss).toContain('.file-pagination { min-height: 58px;')
    expect(themeCss).toContain('height: 100%; margin: 0 auto; padding: 12px 0 34px;')
    expect(themeCss).toContain('overflow: hidden; padding: 26px 28px 30px;')
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toContain('.file-toolbar-actions { width: 100%; }')
    expect(mobile).toContain('.file-row { min-height: 48px; }')
  })

  it('keeps the folder lock indicator smaller than the file icon', () => {
    expect(themeCss).toContain('.file-icon svg { width: 23px; height: 23px; }')
    expect(themeCss).toContain('.file-lock-indicator svg { width: 12px; height: 12px; }')
    expect(themeCss).not.toContain('.lock-dot')
  })

  it('uses matching compact pagination controls and a soft-blue current page', () => {
    expect(themeCss).toContain('.page-size-select { width: 60px; height: 34px;')
    expect(themeCss).toContain('.page-size-select .app-select-trigger { min-height: 34px; height: 34px;')
    expect(themeCss).toContain('.page-size-select .app-select-menu { right: 0; left: auto; width: 100%; }')
    expect(themeCss).toContain('.current-page { flex: 0 0 34px; color: var(--icon-color); background: var(--accent-soft);')
    expect(themeCss).toContain('.page-arrow:disabled { color: var(--muted-2); cursor: default; opacity: 1; }')
  })

  it('keeps browser and security selectors compact without stretching action buttons', () => {
    expect(themeCss).toContain('.browser-storage-switcher .app-select { width: min(176px, 24vw); min-width: 116px; }')
    expect(themeCss).toContain('.security-retention .btn { width: auto; min-width: 132px; min-height: 38px; justify-self: end; }')
  })

  it('keeps the long WebDAV editor inside the mobile viewport with its own scroll', () => {
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toMatch(/\.webdav-modal\s*\{[^}]*max-height: calc\(100dvh - 36px\);[^}]*overflow-y: auto;/s)
    expect(mobile).toContain('-webkit-overflow-scrolling: touch;')
  })

  it('keeps the storage editor wide on desktop', () => {
    expect(themeCss).toMatch(/\.modal\.storage-editor\s*\{[^}]*width: min\(1120px, calc\(100vw - 48px\)\);/s)
  })

  it('moves the new storage action below its description on mobile', () => {
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toMatch(/\.locks-head,\s*\.storage-page-head\s*\{[^}]*flex-direction: column;/s)
    expect(mobile).toMatch(/\.storage-page-head \.btn\s*\{[^}]*width: 100%;[^}]*white-space: nowrap;/s)
  })
})

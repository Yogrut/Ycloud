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

  it('shares home, file browser and admin outer widths on desktop and mobile', () => {
    const sharedRule = /\.login-header,\s*\.login-shell,\s*\.browser-chrome,\s*\.browser-page,\s*\.admin-shell\s*\{([^}]*)\}/g
    const sharedWidths = [...themeCss.matchAll(sharedRule)]
    expect(sharedWidths).toHaveLength(1)
    expect(sharedWidths[0][1]).toContain('width: calc(100% - var(--workspace-gutter) * 2);')
    expect(themeCss).toMatch(/:root\s*\{[^}]*--workspace-gutter: 4.5vw;/)
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toContain(':root { --workspace-gutter: 9px; }')
    // Individual surface rules must not override the common outer grid.
    const individualCss = themeCss.replace(sharedRule, '')
    for (const selector of ['login-header', 'login-shell', 'browser-chrome', 'browser-page', 'admin-shell']) {
      const individualRules = [...individualCss.matchAll(new RegExp('\\.' + selector + '\\s*\\{([^}]*)\\}', 'g'))]
      for (const rule of individualRules) {
        expect(rule[1]).not.toMatch(/(?:^|;)\s*width\s*:/)
      }
    }
  })

  it('preserves Ycloud navigation colours while compacting the layout', () => {
    const layoutCss = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(layoutCss).toMatch(/\.admin-nav\s*\{[^}]*background: var\(--panel\);/)
    expect(layoutCss).toMatch(/\.admin-nav-item\s*\{[^}]*border: 0;[^}]*background: transparent;[^}]*color: var\(--muted\);/)
    expect(layoutCss).toMatch(/\.admin-nav-item\.active\s*\{[^}]*color: var\(--accent\);[^}]*background: var\(--accent-soft\);/)
    expect(layoutCss).not.toContain('.admin-nav-item.active::before')
    expect(layoutCss).not.toContain('.admin-nav-item .app-icon')
  })

  it('keeps normal spacing for masked password glyphs across browsers', () => {
    expect(themeCss).toMatch(/input\[type="password"\]\s*\{[^}]*font-family: var\(--font\);[^}]*font-size: 12px;[^}]*font-weight: 700;[^}]*letter-spacing: normal;/s)
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
    expect(themeCss).not.toContain('width: min(1400px, calc(100% - 36px));')
    expect(themeCss).toContain('.login-card [data-slot="input"] { color: var(--login-text); background: var(--login-field); border-color: transparent;')
    expect(themeCss).toContain('.login-card [data-slot="button"] { border-radius: 9999px; }')
  })

  it('keeps the home content centered at its original width independently of the outer page gutter', () => {
    const mobile = themeCss.match(/@media \(max-width: 560px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    const content = themeCss.match(/\.login-content\s*\{([^}]*)\}/)?.[1] ?? ''
    expect(content).toContain('width: min(1100px, 100%);')
    expect(content).toContain('grid-template-columns: minmax(0, 1fr) minmax(420px, 500px);')
    expect(content).toContain('gap: clamp(48px, 7vw, 120px);')
    expect(themeCss).toMatch(/\.login-shell\s*\{[^}]*margin: 0 auto;[^}]*padding: 64px 0 88px;/)
    expect(themeCss).toMatch(/\.login-shell\s*\{[^}]*place-items: center;/)
    expect(mobile).toContain('.login-page { --workspace-gutter: 16px; }')
    expect(mobile).toContain('.login-shell { min-height: calc(100dvh - 76px); padding: 36px 0 56px; }')
    expect(themeCss).toContain('.login-content { max-width: 500px; grid-template-columns: minmax(0, 1fr);')
    const view = readFileSync('src/apps/login/LoginView.vue', 'utf8')
    expect(view).toContain('login-card w-full max-w-[500px]')
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
    expect(themeCss).toContain('.file-row { min-height: 44px; cursor: default; user-select: none; touch-action: manipulation; }')
    expect(themeCss).toContain('.file-pagination { flex: 0 0 auto; min-height: 44px;')
    expect(themeCss).toContain('height: 100%; margin: 0 auto; padding: 12px 0 34px;')
    expect(themeCss).toContain('overflow: hidden; padding: 14px 28px 14px;')
    expect(themeCss).toMatch(/\.file-toolbar\s*\{[^}]*margin-bottom: 12px;/)
    expect(themeCss).toMatch(/\.breadcrumb\s*\{[^}]*min-height: 32px;[^}]*margin-bottom: 6px;/)
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toContain('.file-toolbar-actions { width: 100%; }')
    expect(mobile).toContain('.file-toolbar { align-items: stretch; flex-direction: column; gap: 8px; margin-bottom: 8px; }')
    expect(mobile).toContain('.breadcrumb { min-height: 32px; margin-bottom: 6px; }')
    expect(mobile).toContain('.file-panel { padding: 10px 14px 12px;')
    expect(mobile).toContain('.file-pagination { min-height: 44px; gap: 10px; padding: 8px 4px 0;')
  })

  it('uses the desktop context menu on every viewport without a bottom sheet or reserved list space', () => {
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(themeCss).toMatch(/\.context-menu\s*\{[^}]*position: fixed;[^}]*z-index: 90;[^}]*width: 168px;/)
    expect(themeCss).toContain('max-height: calc(100dvh - 16px); overflow: auto;')
    expect(mobile).not.toMatch(/\.context-menu|\.menu-item|\.menu-actions|context-open|selection-active/)
  })

  it('keeps the folder lock indicator smaller than the file icon', () => {
    expect(themeCss).toContain('.file-icon img { width: 23px; height: 23px; object-fit: contain; }')
    expect(themeCss).toContain('.file-lock-indicator svg { width: 12px; height: 12px; }')
    expect(themeCss).not.toContain('.lock-dot')
  })

  it('uses matching compact pagination controls and a soft-blue current page', () => {
    const trafficPanel = readFileSync('src/apps/admin/TrafficPanel.vue', 'utf8')
    expect(trafficPanel).toMatch(/\.range-presets button\s*\{[^}]*min-width: 60px; height: 28px; padding: 0 10px;[^}]*font-size: 13px;/)
    expect(themeCss).toContain('.page-size-select { width: 60px; height: 34px;')
    expect(themeCss).toContain('.page-size-select .app-select-trigger { min-height: 34px; height: 34px;')
    expect(themeCss).toContain('.page-size-select .app-select-menu { right: 0; left: auto; width: 100%; }')
    expect(themeCss).toContain('.current-page { flex: 0 0 34px; color: var(--icon-color); background: var(--accent-soft);')
    expect(themeCss).toContain('.page-arrow { padding: 0; color: var(--text); background: transparent; border: 0;')
    expect(themeCss).toContain('.page-arrow:hover:not(:disabled) { color: var(--text); background: transparent; }')
    expect(themeCss).toContain('.page-arrow:disabled { color: var(--muted-2); cursor: default; opacity: 1; }')
    expect(themeCss).toContain('border-top: 2px solid currentColor; border-right: 2px solid currentColor;')
  })

  it('keeps the collapsed music button inside the wide-screen left gutter', () => {
    const browser = readFileSync('src/apps/browser/BrowserView.vue', 'utf8')
    expect(browser).toMatch(/\.ycloud-audio-dock\s*\{[^}]*left: max\(6px, calc\(var\(--workspace-gutter\) \/ 2 - 28px\)\); width: 56px; height: 56px;/)
    expect(browser).toContain('<AppIcon name="vinyl-record" :size="28" weight="fill" />')
    expect(browser).not.toContain('100vw - 1400px')
    expect(browser).not.toContain('@media (min-width: 1536px)')
    expect(browser).toMatch(/\.ycloud-audio-dock summary\s*\{[^}]*width: 100%; height: 100%;/)
    expect(browser).toContain('@media (max-width: 520px) { .ycloud-audio-popover { bottom: calc(100% + 10px); left: 8px; width: calc(100vw - 28px); } }')
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

  it('allocates upload height to the scroll viewport rather than shrinking controls with the task count', () => {
    expect(themeCss).toMatch(/\.upload-window-head,\s*\.upload-add-zone,\s*\.upload-queue-toolbar,\s*\.upload-overall-meta\s*\{\s*flex: 0 0 auto;/)
    expect(themeCss).toMatch(/\.upload-task-list\s*\{[^}]*min-height: 0;[^}]*flex: 1 0 96px;[^}]*overflow-y: auto;/)
    expect(themeCss).toMatch(/\.upload-queue-modal\s*\{[^}]*overflow: auto;/)
    const mobile = themeCss.match(/@media \(max-width: 760px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).not.toMatch(/\.upload-task-list\s*\{[^}]*flex:/)
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

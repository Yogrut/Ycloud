import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const themeCss = readFileSync('src/shared/styles/theme.css', 'utf8')

describe('interactive focus styling', () => {
  it('hides pointer focus rings while retaining a themed keyboard focus indicator', () => {
    expect(themeCss).toContain(':focus:not(:focus-visible)')
    expect(themeCss).toContain(':focus-visible')
    expect(themeCss).toContain('outline: 2px solid var(--accent)')
  })

  it('defines a low-contrast neutral light hierarchy and a separate dark theme', () => {
    expect(themeCss).toContain('--bg: #f3f4f6')
    expect(themeCss).toContain('--panel: #fcfcfd')
    expect(themeCss).toContain('--panel-soft: #f6f7f9')
    expect(themeCss).toContain('--field: #f9fafb')
    expect(themeCss).toContain('--accent: #0874f9')
    expect(themeCss).toContain(':root[data-theme="dark"]')
    expect(themeCss).toContain('--bg: #111722')
    expect(themeCss).toContain('--panel: #171e2a')
  })

  it('keeps the desktop admin content aligned with the navigation panel', () => {
    expect(themeCss).not.toContain('gap: 18px; padding-top: 64px;')
  })

  it('compacts every masked password input consistently', () => {
    expect(themeCss).toMatch(/input\[type="password"\]\s*\{[^}]*letter-spacing: -\.18em;/s)
    expect(themeCss).toMatch(/input\[type="password"\]::placeholder\s*\{[^}]*letter-spacing: normal;/s)
  })

  it('keeps the mobile login card horizontally centered and restores readable password tracking', () => {
    const mobile = themeCss.match(/@media \(max-width: 560px\) \{([\s\S]*?)\n\}/)?.[1] ?? ''
    expect(mobile).toContain('input[type="password"] { letter-spacing: normal; }')
    expect(mobile).toContain('.login-panel { width: 100%; padding: 40px 24px; }')
  })

  it('keeps the file browser compact and exposes mobile actions below search', () => {
    expect(themeCss).toContain('.file-row { min-height: 50px; cursor: default; user-select: none; }')
    expect(themeCss).toContain('.file-pagination { min-height: 58px;')
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

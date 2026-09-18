import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

describe('confirmation consistency', () => {
  it('shares compact footer dimensions with settings drawers and uses themed red warnings', () => {
    const css = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
    expect(css).toMatch(/\.confirmation-actions \.btn, \.settings-drawer \.modal-actions \.btn, \.settings-drawer \.admin-save-row \.btn \{[^}]*height: 32px;[^}]*font-size: 14px;/)
    expect(css).toMatch(/\.confirmation-warning \{[^}]*color: light-dark\(#e05267, #f18a99\);[^}]*background: light-dark\(#fff1f2, #34242a\)/)
    for (const view of ['admin/SecurityView', 'admin/AccountView', 'browser/BrowserView']) {
      const source = readFileSync(`src/apps/${view}.vue`, 'utf8')
      expect(source).toContain('<ConfirmDialog')
      expect(source).not.toMatch(/<div v-if="(?:clearOpen|pending|confirmRemoval|showDelete)" class="overlay/)
    }
  })
})

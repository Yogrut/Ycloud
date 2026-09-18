import { readFileSync } from 'node:fs'
import { expect, it } from 'vitest'

const css = readFileSync('src/shared/styles/settings-layout.css', 'utf8')
it('keeps login spacing independent of inline feedback', () => {
  expect(css).toContain('.admin-login-form-pane.admin-login-form-pane { gap: 16px; }')
  expect(css).toContain('.admin-login-form-pane label { margin-top: 0; }')
  expect(readFileSync('src/shared/styles/theme.css', 'utf8')).toContain('.login-form { display: grid; gap: 20px; }')
})
it('keeps compact sizing on one-field dialogs without shrinking login forms', () => {
  expect(css).toContain('.short-field-dialog .input { height: 32px; min-height: 32px;')
  expect(css).not.toMatch(/\.user-login-form \.input\.input|\.admin-login-form-pane \.input,/)
  expect(css).toContain('.modal.short-field-dialog { width: min(360px, 100%);')
  const view = readFileSync('src/apps/browser/BrowserView.vue', 'utf8')
  for (const action of ['submitFolder', 'submitRename', 'submitUnlock']) {
    expect(view).toContain(`<form class="modal short-field-dialog" @submit.prevent="${action}">`)
  }
})

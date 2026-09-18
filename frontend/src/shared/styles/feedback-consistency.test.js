import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { expect, it } from 'vitest'

it('prevents separate legacy operation notification markup from returning', () => {
  function inspect(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name)
      if (entry.isDirectory()) inspect(path)
      else if (entry.name.endsWith('.vue')) {
        expect(readFileSync(path, 'utf8'), path).not.toMatch(/class="(?:toast|modal-error|login-error|admin-login-error)"/)
      }
    }
  }
  inspect('src')
  expect(readFileSync('src/shared/styles/theme.css', 'utf8')).not.toMatch(/\.toast\s*\{/)
})

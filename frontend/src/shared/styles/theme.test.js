import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const themeCss = readFileSync('src/shared/styles/theme.css', 'utf8')

describe('interactive focus styling', () => {
  it('hides pointer focus rings while retaining a themed keyboard focus indicator', () => {
    expect(themeCss).toContain(':focus:not(:focus-visible)')
    expect(themeCss).toContain(':focus-visible')
    expect(themeCss).toContain('outline: 2px solid var(--accent)')
  })
})

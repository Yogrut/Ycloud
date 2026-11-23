import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const css = readFileSync('src/shared/styles/theme.css', 'utf8')

describe('upload dialog frame', () => {
  it('centers the dialog instead of pinning it to the top of the viewport', () => {
    expect(css).toMatch(/\.upload-window-overlay\s*\{\s*place-items:\s*center;/)
  })

  it('keeps one stable height so adding tasks cannot move a centered dialog', () => {
    const modal = css.match(/\.upload-queue-modal\s*\{([^}]*)\}/s)
    expect(modal).not.toBeNull()
    expect(modal[1]).toMatch(/height:\s*min\(70dvh,\s*620px\)/)
    // A task-count driven custom property would reintroduce the jump.
    expect(modal[1]).not.toContain('--upload-modal-height')
  })

  it('still fills the screen in the narrow layout', () => {
    expect(css).toMatch(/@media \(max-width: 760px\)[\s\S]*?\.upload-queue-modal\s*\{\s*height:\s*calc\(100dvh - 16px\);/)
  })
})

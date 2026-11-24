import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const css = readFileSync('src/shared/styles/theme.css', 'utf8')

describe('upload dialog frame', () => {
  it('stays near the top instead of filling the viewport', () => {
    expect(css).toMatch(/\.upload-window-overlay\s*\{\s*place-items:\s*start center;/)
  })

  it('starts compact and grows with the task count', () => {
    const modal = css.match(/\.upload-queue-modal\s*\{([^}]*)\}/s)
    expect(modal).not.toBeNull()
    // The height comes from the queue length, capped by the viewport.
    expect(modal[1]).toMatch(/height:\s*min\(var\(--upload-modal-height, 390px\), calc\(100dvh - 56px\)\)/)
    expect(modal[1]).toContain('transition: height')
  })

  it('keeps the familiar width on ordinary screens and scales up on wide ones', () => {
    const modal = css.match(/\.upload-queue-modal\s*\{([^}]*)\}/s)
    expect(modal).not.toBeNull()
    // Floor 1280px keeps laptops unchanged; 65vw widens only on big screens,
    // with 1800px as the cap and 100% preventing overflow on narrow ones.
    expect(modal[1]).toMatch(/width:\s*min\(100%, 1800px, max\(1280px, 65vw\)\)/)
  })

  it('still fills the screen in the narrow layout', () => {
    expect(css).toMatch(/@media \(max-width: 760px\)[\s\S]*?\.upload-queue-modal\s*\{\s*height:\s*calc\(100dvh - 16px\);/)
  })
})

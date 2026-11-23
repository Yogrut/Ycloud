import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const css = readFileSync('src/shared/styles/theme.css', 'utf8')

function px(selector) {
  const match = css.match(selector)
  if (!match) throw new Error(`missing ${selector}`)
  return Number(match[1])
}

describe('file list scrollbar', () => {
  it('keeps the bar inside the panel padding at every breakpoint', () => {
    const width = px(/--scrollbar-w:\s*(\d+)px/)
    const gap = px(/--scrollbar-gap:\s*(\d+)px/)
    // Read the narrow breakpoint padding rather than hard-coding today's values.
    const narrow = css.match(/\.file-panel\s*\{\s*padding:\s*10px\s+(\d+)px\s+12px;/)
    expect(narrow).not.toBeNull()
    expect(Number(narrow[1])).toBeGreaterThanOrEqual(width + gap)
  })

  it('reserves the gap as padding so content keeps its aligned width', () => {
    expect(css).toMatch(
      /\.file-list-body\s*\{[^}]*margin-right:\s*calc\(-1 \* \(var\(--scrollbar-w\) \+ var\(--scrollbar-gap\)\)\);/s,
    )
    expect(css).toMatch(/\.file-list-body\s*\{[^}]*padding-right:\s*var\(--scrollbar-gap\);/s)
  })

  it('paints the thumb from its own token in both engines', () => {
    expect(css).toMatch(/\.file-list-body\[data-scrolling\]::-webkit-scrollbar-thumb\s*\{[^}]*background:\s*var\(--scrollbar-thumb\)/s)
    expect(css).toMatch(/\.file-list-body\[data-scrolling\]\s*\{\s*scrollbar-color:\s*var\(--scrollbar-thumb\)\s+transparent;/s)
    // The scrollbar must not fall back to the border token again.
    expect(css).not.toMatch(/scrollbar-thumb[^}]*var\(--line\)/)
    expect(css).not.toMatch(/scrollbar-color:[^;]*var\(--line\)/)
  })

  it('defines the thumb token for both themes', () => {
    expect(css).toMatch(/:root\s*\{[^}]*--scrollbar-thumb:\s*rgb\([^)]*\)/s)
    expect(css).toMatch(/:root\[data-theme="dark"\]\s*\{[^}]*--scrollbar-thumb:\s*rgb\([^)]*\)/s)
  })
})

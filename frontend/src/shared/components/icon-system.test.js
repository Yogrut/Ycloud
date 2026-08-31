import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

function vueFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name)
    return entry.isDirectory() ? vueFiles(path) : entry.name.endsWith('.vue') ? [path] : []
  })
}

describe('shared Phosphor icon system', () => {
  it('does not leave handwritten SVG icons in Vue pages', () => {
    const handwritten = vueFiles('src').filter(path => readFileSync(path, 'utf8').includes('<svg'))
    expect(handwritten).toEqual([])
  })
  it('uses Phosphor Duotone SVGs through the shared wrapper', () => {
    const source = readFileSync('src/shared/components/AppIcon.vue', 'utf8')
    const theme = readFileSync('src/shared/styles/theme.css', 'utf8')
    expect(source).toContain("from '@phosphor-icons/vue'")
    expect(source).toContain('weight="duotone"')
    expect(source).toContain('data-weight="duotone"')
    expect(source).toContain('download: PhArrowCircleDown')
    expect(source).toContain('upload: PhArrowCircleUp')
    expect(source).toContain("'sign-out': PhArrowCircleRight")
    expect(source).not.toContain('material-symbols')
    expect(theme).not.toContain('Material Symbols')
    expect(theme).not.toContain('material-symbols')
    expect(source).not.toContain('@tabler/icons')
  })

  it('uses the selected global icon colour', () => {
    expect(readFileSync('src/shared/styles/theme.css', 'utf8')).toContain('--icon-color: #0564e1')
  })
})

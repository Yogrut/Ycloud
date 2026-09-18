import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { createApp, h } from 'vue'
import AppIcon from './AppIcon.vue'

function vueFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name)
    return entry.isDirectory() ? vueFiles(path) : entry.name.endsWith('.vue') ? [path] : []
  })
}

describe('shared Phosphor icon system', () => {
  it('keeps the favicon identical to the rendered cloud logo without a separate background', () => {
    const favicon = new globalThis.DOMParser().parseFromString(readFileSync('../static/favicon.svg', 'utf8'), 'image/svg+xml').documentElement
    const host = globalThis.document.createElement('div')
    const app = createApp({ render: () => h(AppIcon, { name: 'cloud' }) })
    app.mount(host)
    try {
      const logo = host.querySelector('svg')
      const paths = element => [...element.querySelectorAll('path')].map(path => ({ d: path.getAttribute('d'), opacity: path.getAttribute('opacity') }))
      expect(favicon.getAttribute('viewBox')).toBe(logo.getAttribute('viewBox'))
      expect(paths(favicon)).toEqual(paths(logo))
      expect(favicon.querySelector('rect')).toBeNull()
      const colour = readFileSync('src/shared/styles/theme.css', 'utf8').match(/--icon-color:\s*(#[a-f\d]+)/i)?.[1]
      expect(favicon.getAttribute('fill')).toBe(colour)
      expect(readFileSync('index.html', 'utf8')).toContain('/favicon.svg?v=2')
    } finally { app.unmount() }
  })

  it('does not leave handwritten SVG icons in Vue pages', () => {
    const handwritten = vueFiles('src').filter(path => {
      let source = readFileSync(path, 'utf8')
      // A data-driven circle chart is not an application icon. Keep the icon
      // prohibition everywhere else, including any other SVG in this page.
      if (path === join('src', 'apps', 'admin', 'TrafficPanel.vue')) {
        source = source.replace(/<svg data-chart="traffic-(?:usage|share)"[\s\S]*?<\/svg>/g, '')
      }
      return source.includes('<svg')
    })
    expect(handwritten).toEqual([])
  })
  it('uses Phosphor Duotone SVGs through the shared wrapper', () => {
    const source = readFileSync('src/shared/components/AppIcon.vue', 'utf8')
    const theme = readFileSync('src/shared/styles/theme.css', 'utf8')
    expect(source).toContain("from '@phosphor-icons/vue'")
    expect(source).toContain("weight: 'duotone'")
    expect(source).toContain(':weight="weight"')
    expect(source).toContain(':data-weight="weight"')
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

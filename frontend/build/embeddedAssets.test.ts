import { describe, expect, it } from 'vitest'
import { renderEmbeddedAssets } from './embeddedAssets'

describe('compiled frontend resource table', () => {
  it('is deterministic and assigns explicit JavaScript/CSS types', () => {
    const paths = ['assets/page-AbC.js', 'assets/app.css', 'assets/app.js']
    const source = renderEmbeddedAssets(paths)
    expect(source).toBe(renderEmbeddedAssets([...paths].reverse()))
    expect(source).toContain('static ASSETS: [EmbeddedAsset; 3]')
    expect(source).toContain('EmbeddedAsset::new("app.css", "text/css; charset=utf-8", include_bytes!("assets/app.css"))')
    expect(source).toContain('EmbeddedAsset::new("page-AbC.js", "application/javascript; charset=utf-8", include_bytes!("assets/page-AbC.js"))')
    expect(source.indexOf('"app.css"')).toBeLessThan(source.indexOf('"app.js"'))
  })

  it('rejects duplicate names rather than producing an ambiguous resource table', () => {
    expect(() => renderEmbeddedAssets(['assets/app.js', 'assets/app.js'])).toThrow('Duplicate')
  })

  it('requires the main script and stylesheet before a build can be embedded', () => {
    expect(() => renderEmbeddedAssets(['assets/page-AbC.js'])).toThrow('Missing frontend entry')
  })

  it.each(['assets/page.js.map', 'assets/page.json', 'assets/nested/page.js', 'index.html'])('does not expose unsupported output %s', path => {
    expect(() => renderEmbeddedAssets([path])).toThrow('Unsupported')
  })
})

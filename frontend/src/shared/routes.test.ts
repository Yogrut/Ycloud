import { describe, expect, it } from 'vitest'
import { currentAppPath } from './routes'

describe('application routes', () => {
  it('normalizes candidate and compatibility paths to the production view', () => {
    expect(currentAppPath('/v2/browse')).toBe('/browse')
    expect(currentAppPath('/v2/admin/security')).toBe('/admin/security')
    expect(currentAppPath('/v2/admin/protection')).toBe('/admin/protection')
    expect(currentAppPath('/browser.html')).toBe('/browse')
    expect(currentAppPath('/preview.html')).toBe('/preview')
  })
})

import { describe, expect, it } from 'vitest'
import { appPath, currentAppPath } from './routes'

describe('application routes', () => {
  it('uses the same canonical paths in development and production', () => {
    expect(appPath('/browse')).toBe('/browse')
    expect(appPath('admin/security/')).toBe('/admin/security')
    expect(currentAppPath('/admin/protection/')).toBe('/admin/protection')
    expect(currentAppPath('/index.html')).toBe('/')
  })
})

import { describe, expect, it } from 'vitest'
import { emptyTrafficQuota, sameTrafficQuota, validTrafficQuota } from './trafficQuota'

describe('traffic quota rules', () => {
  it('creates independent unlimited defaults', () => {
    const first = emptyTrafficQuota()
    first.upload = 100
    expect(emptyTrafficQuota()).toEqual({ enabled: false, upload: 0, download: 0 })
  })

  it('compares values rather than field insertion order and distinguishes an absent quota', () => {
    const quota = { enabled: true, upload: 100, download: 200 }
    expect(sameTrafficQuota(quota, { download: 200, upload: 100, enabled: true })).toBe(true)
    expect(sameTrafficQuota(quota, undefined)).toBe(false)
    expect(sameTrafficQuota(quota, { ...quota, enabled: false })).toBe(false)
    expect(sameTrafficQuota(quota, { ...quota, upload: 101 })).toBe(false)
    expect(sameTrafficQuota(quota, { ...quota, download: 201 })).toBe(false)
  })

  it.each([0, 1, Number.MAX_SAFE_INTEGER])('accepts valid byte allowances: %s', value => {
    expect(validTrafficQuota({ enabled: true, upload: value, download: value })).toBe(true)
  })

  it.each([-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1])('rejects invalid bytes even when limits are off: %s', value => {
    expect(validTrafficQuota({ enabled: false, upload: value, download: 0 })).toBe(false)
    expect(validTrafficQuota({ enabled: false, upload: 0, download: value })).toBe(false)
  })
})

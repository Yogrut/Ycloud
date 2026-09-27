import { createApp } from 'vue'
import { describe, expect, it } from 'vitest'
import UserTrafficRings from './UserTrafficRings.vue'

function mountRings(usage: { upload: number; download: number }, quota: { enabled: boolean; upload: number; download: number }) {
  const host = document.createElement('div')
  const app = createApp(UserTrafficRings, { usage, quota })
  app.mount(host)
  return { app, host }
}

describe('UserTrafficRings', () => {
  it.each([false, true])('uses infinity for disabled or zero quotas without hiding usage (%s)', enabled => {
    const { app, host } = mountRings({ upload: 1024, download: 0 }, { enabled, upload: 0, download: 0 })
    try {
      expect(host.querySelector('.traffic-rings-center')?.textContent).toBe('↓∞↑∞')
      expect([...host.querySelectorAll('.traffic-rings-center strong')].map(value => value.textContent)).toEqual(['↓∞', '↑∞'])
      expect(host.querySelector('.traffic-rings-divider')?.textContent).toBe('')
      expect(host.textContent).toContain('1 KiB / ∞')
      expect(host.textContent).toContain('0 B / ∞')
      expect([...host.querySelectorAll('[role="meter"]')].map(meter => meter.getAttribute('aria-valuenow'))).toEqual(['0', '0'])
    } finally { app.unmount() }
  })

  it('clamps a reduced allowance to a full ring and preserves actual usage', () => {
    const { app, host } = mountRings({ upload: 2048, download: 0 }, { enabled: true, upload: 1024, download: 2048 })
    try {
      expect(host.querySelector('[aria-label="上传"]')?.getAttribute('aria-valuenow')).toBe('100')
      expect(host.textContent).toContain('2 KiB / 1 KiB')
    } finally { app.unmount() }
  })
})

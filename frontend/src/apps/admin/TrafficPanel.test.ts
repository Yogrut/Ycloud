import { createApp, nextTick } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { getTraffic, saveTraffic } from '../../shared/api/admin'
import TrafficPanel from './TrafficPanel.vue'

vi.mock('../../shared/api/admin', () => ({ getTraffic: vi.fn(), saveTraffic: vi.fn() }))
const usage = { upload: 1024, download: 2048 }
const fixture = {
  settings: { total: { enabled: true, upload: 4096, download: 8192 }, guest: { enabled: false, upload: 0, download: 0 }, users_total: { enabled: true, upload: 2048, download: 4096 }, users: {}, cycle: { unit: 'months' as const, every: 1, anchor: 1704067200, offset_minutes: 0 } },
  total: usage, guest: { upload: 0, download: 1024 }, users: { a: { upload: 512, download: 512 }, b: { upload: 256, download: 256 } },
  users_total: { upload: 768, download: 768 }, next_reset: 1790812800,
  days: { '2026-09-01': usage, '2026-09-02': { upload: 0, download: 0 } },
}
const apps: ReturnType<typeof createApp>[] = []
let panel: { openEditor: () => Promise<void> } | undefined
async function mount(mode: 'dashboard' | 'settings' = 'dashboard') {
  vi.mocked(getTraffic).mockResolvedValue(structuredClone(fixture))
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(TrafficPanel, { mode }); apps.push(app); panel = app.mount(host) as unknown as typeof panel
  await flush()
  return host
}
async function flush() { await new Promise(resolve => setTimeout(resolve, 0)); await nextTick() }
afterEach(() => { apps.splice(0).forEach(app => app.unmount()); document.body.replaceChildren(); vi.resetAllMocks() })

describe('TrafficPanel', () => {
  it('shows five independent traffic meters and grouped daily bars', async () => {
    const host = await mount()
    expect(host.querySelectorAll('.traffic-card')).toHaveLength(0)
    expect(host.querySelectorAll('.traffic-meter')).toHaveLength(5)
    expect(host.querySelectorAll('[role="meter"]')).toHaveLength(5)
    expect(host.querySelectorAll('svg[data-chart="traffic-usage"]')).toHaveLength(5)
    expect([...host.querySelectorAll('.traffic-meter h3')].map(el => el.textContent)).toEqual(['总流量下载', '总流量上传', '访客下载', '用户下载', '用户上传'])
    expect(host.querySelectorAll('.meter-download')).toHaveLength(3)
    expect(host.querySelectorAll('.meter-upload')).toHaveLength(2)
    expect([...host.querySelectorAll('.meter-amount')].map(el => el.textContent)).toEqual(['2 KiB / 8 KiB', '1 KiB / 4 KiB', '1 KiB / ∞', '768 B / 4 KiB', '768 B / 2 KiB'])
    expect(host.querySelector('.traffic-usage-heading h2')?.textContent).toBe('流量信息')
    expect(host.querySelector('.history-heading h2')?.textContent).toBe('流量统计')
    expect(host.querySelector('.used-amount')).toBeNull()
    expect(host.querySelector('.dial-center span')).toBeNull()
    expect(host.querySelector('.dial-center strong')?.textContent).toMatch(/^\d+(\.\d+)?%$/)
    expect(host.querySelector('.traffic-meters')?.textContent).not.toMatch(/[↑↓]/)
    expect(host.querySelector('.traffic-refresh')).toBeNull()
    expect(host.querySelectorAll('.dashboard-block')).toHaveLength(2)
    expect(host.querySelector('.traffic-usage-block')?.classList.contains('glass')).toBe(true)
    expect(getTraffic).toHaveBeenCalledTimes(1)
    expect(host.querySelector('.meter-track')).toBeNull()
    expect(host.querySelectorAll('.traffic-meter')[3]!.querySelector('[role="meter"]')!.getAttribute('aria-valuetext')).toContain('768 B')
    expect(host.querySelector('.day-bars')!.firstElementChild!.classList.contains('download')).toBe(true)
    expect(host.querySelectorAll('.day-bars')).toHaveLength(2)
    expect(host.querySelectorAll('.day-bars span')).toHaveLength(4)
    expect(host.querySelector('.traffic-totals')).toBeNull()
    expect(host.querySelector('.share-donut')!.textContent).toContain('总流量')
    expect(host.querySelectorAll('.share-ring')).toHaveLength(2)
    expect(host.querySelectorAll('.share-panel .history-legend span')).toHaveLength(2)
    expect(host.querySelector('.traffic-history .traffic-note')?.textContent).toContain('下次统一重置')
    expect(host.querySelector('.traffic-settings')).toBeNull()
    const bars = host.querySelectorAll<HTMLButtonElement>('.day-bars')
    const chart = host.querySelector<HTMLElement>('.bar-chart')!
    vi.spyOn(chart, 'getBoundingClientRect').mockReturnValue({ left: 100, right: 500, top: 100, height: 220 } as DOMRect)
    vi.spyOn(bars[0]!, 'getBoundingClientRect').mockReturnValue({ left: 120, right: 150, top: 100, width: 30, height: 198 } as DOMRect)
    vi.spyOn(bars[1]!, 'getBoundingClientRect').mockReturnValue({ left: 390, right: 420, top: 100, width: 30, height: 198 } as DOMRect)
    bars[0]!.focus(); await nextTick()
    const tooltip = host.querySelector<HTMLElement>('.bar-chart .day-tooltip')!
    expect(tooltip.textContent).toContain('2026-09-01')
    expect(bars[0]!.classList.contains('selected')).toBe(true)
    expect(tooltip.style.left).toBe('60px')
    expect(Number.parseFloat(tooltip.style.left)).toBeGreaterThan(150 - 100)
    bars[1]!.dispatchEvent(new MouseEvent('mouseenter', { clientY: 220 })); await nextTick()
    expect(tooltip.style.left).toBe('106px')
    expect(Number.parseFloat(tooltip.style.left) + Number.parseFloat(tooltip.style.width)).toBeLessThan(390 - 100)
    expect(tooltip.style.top).not.toBe('8px')
    expect(bars[1]!.classList.contains('selected')).toBe(true)
    bars[1]!.dispatchEvent(new MouseEvent('mousemove', { clientY: 260 })); await nextTick()
    expect(tooltip.style.top).toBe('84px')
    expect(host.querySelector('.trend-tooltip-slot')).toBeNull()
    const donut = host.querySelector<HTMLElement>('.share-donut')!
    const sharePanel = host.querySelector<HTMLElement>('.share-panel')!
    vi.spyOn(donut, 'getBoundingClientRect').mockReturnValue({ left: 50, top: 50, width: 200, height: 200 } as DOMRect)
    vi.spyOn(sharePanel, 'getBoundingClientRect').mockReturnValue({ left: 0, width: 330 } as DOMRect)
    donut.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: 150, clientY: 60 })); await nextTick()
    expect(host.querySelector('.share-tooltip')?.textContent).toContain('下载')
    const downloadPosition = host.querySelector<HTMLElement>('.share-tooltip')!.style.left
    expect(host.querySelector('.share-ring-download')?.classList.contains('active')).toBe(true)
    donut.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: 80, clientY: 150 })); await nextTick()
    expect(host.querySelector('.share-tooltip')?.textContent).toContain('上传')
    expect(host.querySelector<HTMLElement>('.share-tooltip')!.style.left).not.toBe(downloadPosition)
    expect(host.querySelector('.share-donut .share-tooltip')).toBeNull()
    donut.dispatchEvent(new MouseEvent('mouseleave')); await nextTick()
    expect(host.querySelector('.share-tooltip')).toBeNull()
    expect(host.querySelector('.share-ring.active')).toBeNull()
    expect(host.querySelector('.traffic-detail')).toBeNull()
    expect(host.querySelector('input[type="date"]')).toBeNull()
    expect(host.querySelector('input[type="datetime-local"]')).toBeNull()
    expect(host.textContent).not.toContain('undefined')
  })
  it('queries a date range without writing quota settings', async () => {
    const host = await mount()
    ;[...host.querySelectorAll<HTMLButtonElement>('.range-presets button')].find(button => button.textContent?.includes('自定义'))!.click()
    await nextTick()
    host.querySelector<HTMLButtonElement>('.traffic-range .date-picker-trigger')!.click()
    await nextTick()
    expect(document.querySelector('.date-picker-popover footer')).toBeNull()
    host.querySelector<HTMLButtonElement>('.traffic-range .date-picker-trigger')!.click()
    host.querySelector('.traffic-range')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await flush()
    const iso = (offset: number) => { const date = new Date(); date.setDate(date.getDate() + offset); return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}` }
    expect(getTraffic).toHaveBeenLastCalledWith(iso(-29), iso(0))
    expect(saveTraffic).not.toHaveBeenCalled()
  })
  it('saves upload/download switches and a shared reset cycle', async () => {
    const host = await mount('settings')
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    await panel!.openEditor(); await flush()
    const drawer = document.querySelector('.settings-drawer')!
    expect(drawer.textContent).toContain('起始日期（本地日期）')
    expect(drawer.querySelector('.time-row')).toBeNull()
    const every = drawer.querySelector<HTMLInputElement>('.cycle-fields input[type="number"]')!
    every.value = '2'; every.dispatchEvent(new Event('input'))
    drawer.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }))
    await flush()
    expect(saveTraffic).toHaveBeenCalledWith(expect.objectContaining({
      total: fixture.settings.total, guest: fixture.settings.guest, users_total: fixture.settings.users_total,
      cycle: expect.objectContaining({ every: 2, unit: 'months' }),
    }))
    expect(host.querySelector('.traffic-history')).toBeNull()
  })
  it('does not include per-account allowances in transfer limits', async () => {
    await mount('settings')
    await panel!.openEditor(); await flush()
    const drawer = document.querySelector('.settings-drawer')!
    expect(drawer.textContent).not.toContain('账号独立额度')
    expect(drawer.querySelector('.account-quota-row')).toBeNull()
  })
})

import { createApp } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import DashboardView from './DashboardView.vue'

vi.mock('./TrafficPanel.vue', () => ({ default: { props: ['mode'], template: '<div class="traffic-stub">{{ mode }}</div>' } }))
const apps: ReturnType<typeof createApp>[] = []
afterEach(() => {
  apps.splice(0).forEach(app => app.unmount())
  document.body.replaceChildren()
})

describe('DashboardView', () => {
  it('renders project overview before the dashboard traffic sections', () => {
    const host = document.createElement('div')
    document.body.append(host)
    const app = createApp(DashboardView, {
      info: {
        storage_instances: [{}, {}],
        user_accounts: [{}, {}, {}],
        shares: [{}],
        folder_locks: [{}, {}],
      },
    })
    apps.push(app); app.mount(host)
    expect(host.querySelector('#dashboard-title')?.textContent).toBe('仪表盘')
    expect(host.querySelector('.overview-section h2')?.textContent).toBe('概览')
    expect([...host.querySelectorAll('.overview-grid strong')].map(element => element.textContent)).toEqual(['2', '3', '1', '2'])
    expect(host.querySelector('.traffic-stub')?.textContent).toBe('dashboard')
  })
})

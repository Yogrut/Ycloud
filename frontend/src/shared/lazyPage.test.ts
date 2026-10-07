import { createApp, h, nextTick, type Component } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { lazyPage } from './lazyPage'

async function settle(): Promise<void> {
  for (let turn = 0; turn < 8; turn++) await Promise.resolve()
  await nextTick()
}

function mountPage(component: Component) {
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(component)
  const errors = vi.fn()
  app.config.errorHandler = errors
  app.mount(host)
  return { app, host, errors }
}

afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
  document.body.replaceChildren()
})

describe('lazy page lifecycle', () => {
  it('loads only when selected, shows progress, and reuses resolved code on a later mount', async () => {
    let resolve!: (component: Component) => void
    const loader = vi.fn(() => new Promise<Component>(done => { resolve = done }))
    const page = lazyPage(loader)
    expect(loader).not.toHaveBeenCalled()
    const first = mountPage(page)
    try {
      expect(first.host.querySelector('[role="status"]')).not.toBeNull()
      expect(loader).toHaveBeenCalledTimes(1)
      resolve({ render: () => h('section', 'Loaded page') })
      await settle()
      expect(first.host.textContent).toBe('Loaded page')
    } finally { first.app.unmount() }
    const second = mountPage(page)
    try {
      await settle()
      expect(second.host.textContent).toBe('Loaded page')
      expect(loader).toHaveBeenCalledTimes(1)
    } finally { second.app.unmount() }
  })

  it('shows a safe error and offers manual reload without retrying or reloading automatically', async () => {
    const reload = vi.spyOn(window.location, 'reload').mockImplementation(() => undefined)
    const loader = vi.fn().mockRejectedValue(new Error('private network details'))
    const mounted = mountPage(lazyPage(loader))
    try {
      await settle()
      expect(mounted.host.querySelector('[role="alert"]')).not.toBeNull()
      expect(mounted.host.textContent).not.toContain('private network details')
      expect(loader).toHaveBeenCalledTimes(1)
      expect(reload).not.toHaveBeenCalled()
      mounted.host.querySelector<HTMLButtonElement>('button')!.click()
      expect(reload).toHaveBeenCalledTimes(1)
    } finally { mounted.app.unmount() }
  })

  it('reports an unfinished code load after its deadline without retrying', async () => {
    vi.useFakeTimers()
    const loader = vi.fn(() => new Promise<Component>(() => undefined))
    const mounted = mountPage(lazyPage(loader))
    try {
      await vi.advanceTimersByTimeAsync(30_000)
      await settle()
      expect(mounted.host.querySelector('[role="alert"]')).not.toBeNull()
      expect(loader).toHaveBeenCalledTimes(1)
    } finally { mounted.app.unmount() }
  })

  it('does not mount a late result after the selected page has been unmounted', async () => {
    let resolve!: (component: Component) => void
    const setup = vi.fn(() => () => h('section', 'Late page'))
    const mounted = mountPage(lazyPage(() => new Promise<Component>(done => { resolve = done })))
    mounted.app.unmount()
    resolve({ setup })
    await settle()
    expect(setup).not.toHaveBeenCalled()
    expect(mounted.host.textContent).toBe('')
  })
})

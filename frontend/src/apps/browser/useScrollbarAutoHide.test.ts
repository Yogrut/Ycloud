import { createApp, defineComponent, h, nextTick, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { SCROLLBAR_IDLE_MS, useScrollbarAutoHide } from './useScrollbarAutoHide'

const Host = defineComponent({
  setup() {
    const panel = ref<HTMLElement>()
    useScrollbarAutoHide(panel)
    return () => h('div', { ref: panel }, [h('div', { class: 'scroller' })])
  },
})

async function mountHost() {
  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(Host)
  app.mount(host)
  await nextTick()
  return { app, scroller: host.querySelector<HTMLElement>('.scroller')! }
}

afterEach(() => {
  vi.useRealTimers()
  document.body.replaceChildren()
})

describe('scrollbar auto hide', () => {
  it('marks the scroller while scrolling and clears it after the idle period', async () => {
    vi.useFakeTimers()
    const { app, scroller } = await mountHost()
    try {
      scroller.dispatchEvent(new Event('scroll'))
      expect(scroller.hasAttribute('data-scrolling')).toBe(true)
      await vi.advanceTimersByTimeAsync(SCROLLBAR_IDLE_MS)
      expect(scroller.hasAttribute('data-scrolling')).toBe(false)
    } finally { app.unmount() }
  })

  it('restarts the idle period on every scroll', async () => {
    vi.useFakeTimers()
    const { app, scroller } = await mountHost()
    try {
      scroller.dispatchEvent(new Event('scroll'))
      await vi.advanceTimersByTimeAsync(SCROLLBAR_IDLE_MS - 100)
      scroller.dispatchEvent(new Event('scroll'))
      await vi.advanceTimersByTimeAsync(SCROLLBAR_IDLE_MS - 100)
      expect(scroller.hasAttribute('data-scrolling')).toBe(true)
      await vi.advanceTimersByTimeAsync(200)
      expect(scroller.hasAttribute('data-scrolling')).toBe(false)
    } finally { app.unmount() }
  })

  it('clears the pending timer and the mark when the panel goes away', async () => {
    vi.useFakeTimers()
    const { app, scroller } = await mountHost()
    scroller.dispatchEvent(new Event('scroll'))
    app.unmount()
    expect(scroller.hasAttribute('data-scrolling')).toBe(false)
    await vi.advanceTimersByTimeAsync(SCROLLBAR_IDLE_MS * 2)
    expect(vi.getTimerCount()).toBe(0)
  })
})

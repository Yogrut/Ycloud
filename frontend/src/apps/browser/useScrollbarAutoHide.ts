import { onBeforeUnmount, onMounted, type Ref } from 'vue'

/// How long the scrollbar stays visible after the last scroll event.
export const SCROLLBAR_IDLE_MS = 3000

/**
 * Marks the scrolling element with `data-scrolling` while it scrolls and drops
 * the mark after a short idle period, so CSS can show a neutral bar only then.
 * A capture listener covers every descendant scroller of the panel.
 */
export function useScrollbarAutoHide(panel: Ref<HTMLElement | undefined>): void {
  let timer: ReturnType<typeof setTimeout> | undefined
  let current: HTMLElement | undefined

  function clear(element: HTMLElement | undefined): void {
    element?.removeAttribute('data-scrolling')
  }

  function onScroll(event: Event): void {
    const target = event.target
    if (!(target instanceof HTMLElement)) return
    if (current && current !== target) clear(current)
    current = target
    target.setAttribute('data-scrolling', '')
    if (timer !== undefined) clearTimeout(timer)
    timer = setTimeout(() => {
      timer = undefined
      clear(current)
      current = undefined
    }, SCROLLBAR_IDLE_MS)
  }

  onMounted(() => panel.value?.addEventListener('scroll', onScroll, true))
  onBeforeUnmount(() => {
    panel.value?.removeEventListener('scroll', onScroll, true)
    if (timer !== undefined) clearTimeout(timer)
    clear(current)
  })
}

import { onBeforeUnmount, onMounted, shallowReactive, watch, type Ref } from 'vue'
import { thumbnailUrl, type FileEntry } from '../../shared/api/browser'
import { fetchGalleryThumbnail, type ThumbnailStatus } from './galleryImageResource'

const SCROLL_IDLE_FALLBACK_MS = 120
const MAX_ACTIVE = 3
const PENDING_INTERVAL_MS = 2_000

export function galleryImageKey(entry: FileEntry, storageId: string): string {
  return JSON.stringify([storageId, entry.path, entry.size, entry.modified_unix ?? entry.modified])
}

export function useGalleryThumbnails(
  grid: Ref<HTMLElement | null>,
  candidates: Readonly<Ref<ReadonlyMap<string, FileEntry>>>,
  options: () => { storageId: string; paused?: boolean },
) {
  const sources = shallowReactive(new Map<string, string>())
  const ratios = shallowReactive(new Map<string, number>())
  const observed = new Map<HTMLElement, string>()
  const visible = new Set<string>()
  const status = shallowReactive(new Map<string, ThumbnailStatus>())
  const pendingUntil = new Map<string, number>()
  let observer: IntersectionObserver | undefined
  let scrollRoot: HTMLElement | null = null
  const active = new Map<string, AbortController>()
  let frame: number | undefined
  let idleTimer: ReturnType<typeof setTimeout> | undefined
  let pendingTimer: ReturnType<typeof setTimeout> | undefined
  let fastScroll = false
  let lastTop = 0
  let lastScroll = 0
  let disposed = false

  function dropSource(key: string): void {
    const source = sources.get(key)
    if (source) URL.revokeObjectURL(source)
    sources.delete(key)
  }

  function schedule(): void {
    if (disposed || frame !== undefined) return
    frame = requestAnimationFrame(() => {
      frame = undefined
      reconcile()
    })
  }

  function reconcile(): void {
    clearTimeout(pendingTimer)
    const allowed = candidates.value
    const { paused } = options()
    for (const key of sources.keys()) {
      if (!visible.has(key) || !allowed.has(key)) dropSource(key)
    }
    for (const [key, controller] of active) {
      if (paused || fastScroll || !visible.has(key) || !allowed.has(key)) controller.abort()
    }
    if (paused || fastScroll) return
    let nextCheck = Infinity
    for (const key of visible) {
      const entry = allowed.get(key)
      if (!entry || sources.has(key) || active.has(key)) continue
      const currentStatus = status.get(key)
      if (currentStatus && currentStatus !== 'pending') continue
      const waitUntil = pendingUntil.get(key)
      if (waitUntil !== undefined && waitUntil > performance.now()) {
        nextCheck = Math.min(nextCheck, waitUntil)
        continue
      }
      if (active.size < MAX_ACTIVE) void load(key, entry)
    }
    if (Number.isFinite(nextCheck)) {
      pendingTimer = setTimeout(schedule, Math.max(0, nextCheck - performance.now()))
    }
  }

  async function load(key: string, entry: FileEntry): Promise<void> {
    const controller = new AbortController()
    active.set(key, controller)
    try {
      const result = await fetchGalleryThumbnail(thumbnailUrl(entry.path, options().storageId), controller.signal)
      if (controller.signal.aborted || disposed || !visible.has(key) || !candidates.value.has(key) || options().paused) return
      if (result.status !== 'ready') {
        status.set(key, result.status)
        if (result.status === 'pending') pendingUntil.set(key, performance.now() + PENDING_INTERVAL_MS)
        else pendingUntil.delete(key)
        return
      }
      const { thumb } = result
      status.delete(key)
      pendingUntil.delete(key)
      ratios.set(key, thumb.ratio)
      sources.set(key, URL.createObjectURL(thumb.bytes))
    } catch {
      if (!controller.signal.aborted) status.set(key, 'network')
    } finally {
      active.delete(key)
      schedule()
    }
  }

  function syncCards(): void {
    if (!grid.value || !observer) return
    const nodes = new Set(grid.value.querySelectorAll<HTMLElement>('[data-gallery-key]'))
    for (const [node, key] of observed) {
      if (nodes.has(node)) continue
      observer.unobserve(node)
      observed.delete(node)
      visible.delete(key)
    }
    for (const node of nodes) {
      if (observed.has(node)) continue
      const key = node.dataset.galleryKey
      if (!key) continue
      observed.set(node, key)
      observer.observe(node)
    }
    for (const key of ratios.keys()) if (!candidates.value.has(key)) ratios.delete(key)
    for (const key of status.keys()) {
      if (candidates.value.has(key)) continue
      status.delete(key)
      pendingUntil.delete(key)
    }
    reconcile()
  }

  function settleScroll(): void {
    fastScroll = false
    lastScroll = 0
    schedule()
  }

  function onScroll(): void {
    if (!scrollRoot) return
    const top = scrollRoot.scrollTop
    const now = performance.now()
    const distance = Math.abs(top - lastTop)
    const viewport = Math.max(1, scrollRoot.clientHeight)
    if (distance > viewport / 2 || (lastScroll && distance / Math.max(16, now - lastScroll) > viewport / 200)) {
      fastScroll = true
      for (const controller of active.values()) controller.abort()
    }
    lastTop = top
    lastScroll = now
    if (!('onscrollend' in scrollRoot)) {
      clearTimeout(idleTimer)
      idleTimer = setTimeout(settleScroll, SCROLL_IDLE_FALLBACK_MS)
    }
  }

  onMounted(() => {
    scrollRoot = grid.value?.closest<HTMLElement>('.gallery-list-body') ?? null
    lastTop = scrollRoot?.scrollTop ?? 0
    scrollRoot?.addEventListener('scroll', onScroll, { passive: true })
    scrollRoot?.addEventListener('scrollend', settleScroll)
    observer = new IntersectionObserver(entries => {
      for (const entry of entries) {
        const key = observed.get(entry.target as HTMLElement)
        if (!key) continue
        if (entry.isIntersecting && entry.intersectionRatio >= 0.01) visible.add(key)
        else {
          visible.delete(key)
          dropSource(key)
          active.get(key)?.abort()
          if (status.get(key) === 'network') status.delete(key)
        }
      }
      schedule()
    }, { root: scrollRoot, threshold: 0.01 })
    syncCards()
  })
  watch(candidates, (allowed, previous) => {
    // A new listing is the boundary for revalidating previously displayed
    // thumbnails. The disk cache belongs to the authenticated endpoint.
    for (const [key, entry] of allowed) {
      if (entry === previous.get(key)) continue
      dropSource(key)
      active.get(key)?.abort()
      status.delete(key)
      pendingUntil.delete(key)
    }
    syncCards()
  }, { flush: 'post' })
  watch(() => options().paused, paused => {
    if (paused) {
      clearTimeout(pendingTimer)
      for (const controller of active.values()) controller.abort()
    }
    else schedule()
  })
  onBeforeUnmount(() => {
    disposed = true
    if (frame !== undefined) cancelAnimationFrame(frame)
    clearTimeout(idleTimer)
    clearTimeout(pendingTimer)
    scrollRoot?.removeEventListener('scroll', onScroll)
    scrollRoot?.removeEventListener('scrollend', settleScroll)
    observer?.disconnect()
    for (const controller of active.values()) controller.abort()
    for (const key of sources.keys()) dropSource(key)
  })

  return { sources, ratios, status }
}

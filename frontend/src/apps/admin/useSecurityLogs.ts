import { computed, onScopeDispose, ref, watch } from 'vue'
import type { LoginEntry, LoginEvent } from '../../shared/api/admin'
import { getLoginEvents } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

export const LOG_PAGE_SIZES = [20, 50, 100] as const
export const LOG_REFRESH_SECONDS = [0, 30, 60, 300] as const
const PAGE_NEIGHBORS = 2
const MILLISECONDS_PER_SECOND = 1000

export function useSecurityLogs(canAutoRefresh: () => boolean) {
  const locale = useLocale()
  const kind = ref<'' | 'normal' | 'error'>('')
  const entry = ref<LoginEntry | ''>('')
  const ip = ref('')
  const queryText = ref('')
  const events = ref<LoginEvent[]>([])
  const loading = ref(false)
  const loadError = ref('')
  const pageNumber = ref(1)
  const pageSize = ref<number>(LOG_PAGE_SIZES[0])
  const total = ref(0)
  const jumpPage = ref<string | number>(1)
  const refreshSeconds = ref<number>(0)
  let requestVersion = 0
  let request: AbortController | undefined
  let timer: ReturnType<typeof setInterval> | undefined
  let disposed = false

  const totalPages = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value)))
  const visiblePages = computed(() => {
    const neighbors = Array.from({ length: PAGE_NEIGHBORS * 2 + 1 }, (_, index) => pageNumber.value - PAGE_NEIGHBORS + index)
    const pages = [...new Set([1, ...neighbors, totalPages.value])]
      .filter(page => page >= 1 && page <= totalPages.value).sort((a, b) => a - b)
    return pages.flatMap((page, index) => index && page - pages[index - 1]! > 1 ? ['…', page] : [page])
  })

  function invalidate(): void {
    ++requestVersion
    request?.abort()
    request = undefined
    loading.value = false
  }

  async function load(page = pageNumber.value): Promise<void> {
    if (disposed) return
    const version = ++requestVersion
    request?.abort()
    const controller = new AbortController()
    request = controller
    loading.value = true
    loadError.value = ''
    try {
      const result = await getLoginEvents({
        success: kind.value ? kind.value === 'normal' : undefined,
        entry: entry.value || undefined,
        search: queryText.value,
        page,
        limit: pageSize.value,
      }, controller.signal)
      // Cancellation saves work; the version also rejects late responses from
      // transports that already completed or did not honor cancellation.
      if (version !== requestVersion) return
      events.value = result.events
      total.value = result.total
      pageNumber.value = result.page
      jumpPage.value = result.page
    } catch (error) {
      if (version === requestVersion) loadError.value = error instanceof Error ? error.message : locale.text('登录日志加载失败', 'Unable to load sign-in logs')
    } finally {
      if (version === requestVersion) {
        request = undefined
        loading.value = false
      }
    }
  }

  function resetAndLoad(): void {
    queryText.value = ip.value.trim()
    void load(1)
  }

  function goToPage(page: number): void {
    if (loading.value || !Number.isInteger(page) || page < 1 || page > totalPages.value) return
    void load(page)
  }

  watch([kind, entry, pageSize], resetAndLoad)
  watch(refreshSeconds, seconds => {
    clearInterval(timer)
    timer = undefined
    if (seconds) timer = setInterval(() => {
      if (!document.hidden && !loading.value && canAutoRefresh() && pageNumber.value === 1) void load(1)
    }, seconds * MILLISECONDS_PER_SECOND)
  })
  onScopeDispose(() => {
    disposed = true
    clearInterval(timer)
    invalidate()
  })

  return {
    kind, entry, ip, events, loading, loadError, pageNumber, pageSize, total, jumpPage,
    refreshSeconds, totalPages, visiblePages, load, invalidate, resetAndLoad, goToPage,
  }
}

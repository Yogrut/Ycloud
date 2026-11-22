import { onScopeDispose, watch, type Ref } from 'vue'
import { changesUrl } from '../../shared/api/browser'

const DEBOUNCE_MS = 300

/**
 * Follows the directory currently on screen over server-sent events. The server
 * pushes one signal per changed directory; this hook turns a burst into a single
 * silent refresh, so rows appear or disappear without any prompt or spinner.
 */
export function useDirectoryWatcher(
  storageId: Ref<string>,
  path: Ref<string>,
  onChange: () => void,
): void {
  let source: EventSource | undefined
  let timer: ReturnType<typeof setTimeout> | undefined

  function schedule(): void {
    if (timer !== undefined) return
    timer = setTimeout(() => {
      timer = undefined
      onChange()
    }, DEBOUNCE_MS)
  }

  let paused = false
  let disposed = false
  let retry = 0
  let failures = 0
  let retryTimer: ReturnType<typeof setTimeout> | undefined

  // EventSource cannot see the status code, and a 403 (folder locked, session
  // revoked) closes it exactly like a fatal error. Retrying forever would hammer
  // the server, so give up after a few consecutive closed attempts.
  const MAX_RECONNECT_FAILURES = 3

  // A 503 (stream budget full) or 401 (session expired) closes the connection for
  // good — browsers only retry network drops — so reconnect ourselves, backing off.
  function scheduleReconnect(): void {
    if (disposed || retryTimer !== undefined) return
    if (failures >= MAX_RECONNECT_FAILURES) return
    failures += 1
    const backoff = Math.min(30_000, 1_000 * 2 ** Math.min(retry, 5))
    const delay = backoff * (0.5 + Math.random() * 0.5)
    retry += 1
    retryTimer = setTimeout(() => {
      retryTimer = undefined
      if (!disposed && document.visibilityState === 'visible') start()
    }, delay)
  }

  function stop(): void {
    source?.close()
    source = undefined
    if (timer !== undefined) {
      clearTimeout(timer)
      timer = undefined
    }
    if (retryTimer !== undefined) {
      clearTimeout(retryTimer)
      retryTimer = undefined
    }
  }

  // A hidden tab releases the stream: browsers cap connections per origin and
  // nothing needs to be pushed to a page nobody is looking at.
  function handleVisibility(): void {
    if (document.visibilityState === 'hidden') {
      // Also stop a pending reconnect: it would fire while hidden, skip the
      // reconnect, and leave nothing to resume from.
      stop()
      paused = true
      return
    }
    if (!paused) return
    paused = false
    // Coming back to the tab is a fresh chance: a previous lock may be gone.
    failures = 0
    start()
    // Events may have been missed while the stream was closed.
    schedule()
  }

  function start(): void {
    stop()
    // Browsers without EventSource (older embeddings, jsdom) simply keep the
    // manual refresh behaviour.
    if (typeof EventSource === 'undefined') return
    // The listing resolves the storage id after mount; subscribing before that
    // would open a throwaway connection with no storage scope.
    if (!storageId.value) return
    let opened = false
    const target = new EventSource(changesUrl(path.value, storageId.value || undefined))
    target.addEventListener('change', schedule)
    // EventSource reconnects on its own, but events sent while it was down are
    // lost; one refresh after a reconnect brings the page up to date.
    target.addEventListener('open', () => {
      retry = 0
      failures = 0
      if (opened) schedule()
      opened = true
    })
    target.addEventListener('error', () => {
      // A network drop or the server ending the stream leaves the source
      // CONNECTING (0) and the browser retries on its own — never race it with
      // our own reconnect. Only a non-200 answer closes it (2 = CLOSED), and
      // browsers do not retry those.
      if (target.readyState !== 2 /* EventSource.CLOSED */) return
      target.close()
      if (source === target) source = undefined
      scheduleReconnect()
    })
    source = target
  }

  // A new directory deserves a fresh reconnect budget: a locked folder must not
  // poison the next one.
  watch([storageId, path], () => {
    failures = 0
    start()
  }, { immediate: true })
  document.addEventListener('visibilitychange', handleVisibility)
  onScopeDispose(() => {
    disposed = true
    document.removeEventListener('visibilitychange', handleVisibility)
    stop()
  })
}

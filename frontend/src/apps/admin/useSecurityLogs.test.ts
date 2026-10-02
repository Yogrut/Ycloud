import { effectScope, nextTick, type EffectScope } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { LoginEvent, LoginEventPage } from '../../shared/api/admin'
import { getLoginEvents } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { useSecurityLogs } from './useSecurityLogs'

vi.mock('../../shared/api/admin', () => ({ getLoginEvents: vi.fn() }))
const getEvents = vi.mocked(getLoginEvents)
const scopes: EffectScope[] = []
const event: LoginEvent = {
  id: 1, entry: 'admin', success: true, ip: '192.0.2.1', occurred_at: 100,
  result: '登录成功', failed_attempts: 0, blocked_until: null, user_agent: null, current_blocked_until: null,
}

function result(page = 1, total = 100, id = 1): LoginEventPage {
  return { events: [{ ...event, id }], page, total, next_cursor: null }
}

function deferred() {
  let resolve!: (value: LoginEventPage) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<LoginEventPage>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup(canAutoRefresh = () => true) {
  const scope = effectScope()
  scopes.push(scope)
  const logs = scope.run(() => useSecurityLogs(canAutoRefresh))!
  return { logs, scope }
}

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop()
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  getEvents.mockReset()
  useLocale().set('zh-CN')
})

describe('security log queries', () => {
  it('loads one bounded page and applies the server page, count and jump value', async () => {
    getEvents.mockResolvedValue(result(3, 143))
    const { logs } = setup()
    expect(getEvents).not.toHaveBeenCalled()
    await logs.load(3)
    expect(getEvents).toHaveBeenCalledWith({ success: undefined, entry: undefined, search: '', page: 3, limit: 20 }, expect.any(AbortSignal))
    expect(logs.events.value).toEqual(result().events)
    expect(logs.pageNumber.value).toBe(3)
    expect(logs.jumpPage.value).toBe(3)
    expect(logs.total.value).toBe(143)
    expect(logs.totalPages.value).toBe(8)
    expect(logs.loading.value).toBe(false)
  })

  it('freezes submitted search text until search or a filter change is applied', async () => {
    getEvents.mockResolvedValue(result())
    const { logs } = setup()
    logs.ip.value = '  Browser A  '
    logs.resetAndLoad()
    await nextTick()
    logs.ip.value = 'Browser B'
    await logs.load(2)
    expect(getEvents.mock.calls.at(-1)?.[0]).toMatchObject({ search: 'Browser A', page: 2 })
    logs.kind.value = 'error'
    logs.entry.value = 'web_dav'
    logs.pageSize.value = 50
    await nextTick()
    expect(getEvents.mock.calls.at(-1)?.[0]).toEqual({ success: false, entry: 'web_dav', search: 'Browser B', page: 1, limit: 50 })
    expect(getEvents).toHaveBeenCalledTimes(3)
  })

  it('requests successful events separately and clears filters without changing search semantics', async () => {
    getEvents.mockResolvedValue(result())
    const { logs } = setup()
    logs.kind.value = 'normal'
    logs.entry.value = 'account'
    await nextTick()
    expect(getEvents.mock.calls.at(-1)?.[0]).toMatchObject({ success: true, entry: 'account', page: 1 })
    logs.kind.value = ''
    logs.entry.value = ''
    await nextTick()
    expect(getEvents.mock.calls.at(-1)?.[0]).toMatchObject({ success: undefined, entry: undefined })
  })

  it.each(['success', 'error'] as const)('ignores a stale %s without ending the latest loading state', async outcome => {
    const old = deferred()
    const current = deferred()
    getEvents.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise)
    const { logs } = setup()
    const first = logs.load(1)
    const oldSignal = getEvents.mock.calls[0]![1]!
    const second = logs.load(2)
    expect(oldSignal.aborted).toBe(true)
    if (outcome === 'success') old.resolve(result(1, 500, 11))
    else old.reject(new Error('Old read failed'))
    await first
    expect(logs.loading.value).toBe(true)
    expect(logs.events.value).toEqual([])
    expect(logs.loadError.value).toBe('')
    current.resolve(result(2, 100, 22))
    await second
    expect(logs.events.value[0]?.id).toBe(22)
    expect(logs.pageNumber.value).toBe(2)
    expect(logs.loading.value).toBe(false)
  })

  it('does not let a late success overwrite the latest failure', async () => {
    const old = deferred()
    getEvents.mockReturnValueOnce(old.promise).mockRejectedValueOnce(new Error('Latest read failed'))
    const { logs } = setup()
    const first = logs.load(1)
    await logs.load(2)
    old.resolve(result(1, 100, 11))
    await first
    expect(logs.loadError.value).toBe('Latest read failed')
    expect(logs.events.value).toEqual([])
    expect(logs.loading.value).toBe(false)
  })

  it('invalidates a read before clearing logs and ignores its late result', async () => {
    const pending = deferred()
    getEvents.mockReturnValueOnce(pending.promise).mockResolvedValueOnce(result(1, 0))
    const { logs } = setup()
    const read = logs.load()
    const signal = getEvents.mock.calls[0]![1]!
    logs.invalidate()
    expect(signal.aborted).toBe(true)
    expect(logs.loading.value).toBe(false)
    pending.resolve(result())
    await read
    expect(logs.events.value).toEqual([])
    await logs.load(1)
    expect(logs.total.value).toBe(0)
  })

  it('keeps loaded rows on failure, exposes the error and clears it on a new read', async () => {
    getEvents.mockResolvedValueOnce(result()).mockRejectedValueOnce(new Error('Read failed')).mockResolvedValueOnce(result(2))
    const { logs } = setup()
    await logs.load()
    await logs.load(2)
    expect(logs.events.value).toEqual(result().events)
    expect(logs.loadError.value).toBe('Read failed')
    await logs.load(2)
    expect(logs.loadError.value).toBe('')
    expect(logs.pageNumber.value).toBe(2)
  })

  it('localizes non-Error failures using the existing locale', async () => {
    useLocale().set('en')
    getEvents.mockRejectedValue('read failed')
    const { logs } = setup()
    await logs.load()
    expect(logs.loadError.value).toBe('Unable to load sign-in logs')
  })

  it('validates page jumps without queuing them while a read is pending', async () => {
    getEvents.mockResolvedValueOnce(result(1, 100))
    const { logs } = setup()
    await logs.load()
    getEvents.mockClear()
    for (const page of [0, -1, 1.5, 6, NaN, Infinity]) logs.goToPage(page)
    expect(getEvents).not.toHaveBeenCalled()
    const pending = deferred()
    getEvents.mockReturnValueOnce(pending.promise)
    logs.goToPage(3)
    logs.goToPage(4)
    expect(getEvents).toHaveBeenCalledTimes(1)
    pending.resolve(result(3))
    await nextTick()
    expect(logs.pageNumber.value).toBe(3)
  })

  it.each([
    [0, 1, [1]],
    [400, 1, [1, 2, 3, '…', 20]],
    [400, 10, [1, '…', 8, 9, 10, 11, 12, '…', 20]],
    [400, 20, [1, '…', 18, 19, 20]],
  ] as const)('keeps compact page navigation for total %i at page %i', (total, page, visible) => {
    const { logs } = setup()
    logs.total.value = total
    logs.pageNumber.value = page
    expect(logs.visiblePages.value).toEqual(visible)
  })

  it('aborts on disposal and never starts follow-up reads or applies late results', async () => {
    const pending = deferred()
    getEvents.mockReturnValueOnce(pending.promise)
    const { logs, scope } = setup()
    const read = logs.load()
    const signal = getEvents.mock.calls[0]![1]!
    scope.stop()
    expect(signal.aborted).toBe(true)
    pending.resolve(result())
    await read
    await logs.load(2)
    logs.resetAndLoad()
    expect(getEvents).toHaveBeenCalledTimes(1)
    expect(logs.events.value).toEqual([])
    expect(logs.loading.value).toBe(false)
  })
})

describe('security log auto-refresh', () => {
  it('replaces intervals, disables refresh and disposes the final timer', async () => {
    vi.useFakeTimers()
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false)
    getEvents.mockResolvedValue(result())
    const { logs, scope } = setup()
    logs.refreshSeconds.value = 30
    await nextTick()
    expect(vi.getTimerCount()).toBe(1)
    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEvents).toHaveBeenCalledTimes(1)
    logs.refreshSeconds.value = 60
    await nextTick()
    expect(vi.getTimerCount()).toBe(1)
    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEvents).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(30_000)
    expect(getEvents).toHaveBeenCalledTimes(2)
    logs.refreshSeconds.value = 0
    await nextTick()
    expect(vi.getTimerCount()).toBe(0)
    logs.refreshSeconds.value = 30
    await nextTick()
    scope.stop()
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(60_000)
    expect(getEvents).toHaveBeenCalledTimes(2)
  })

  it.each(['hidden', 'loading', 'dialog', 'later_page'] as const)('does not poll while %s', async reason => {
    vi.useFakeTimers()
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(reason === 'hidden')
    const { logs } = setup(() => reason !== 'dialog')
    logs.loading.value = reason === 'loading'
    logs.pageNumber.value = reason === 'later_page' ? 2 : 1
    logs.refreshSeconds.value = 30
    await nextTick()
    await vi.advanceTimersByTimeAsync(90_000)
    expect(getEvents).not.toHaveBeenCalled()
  })
})

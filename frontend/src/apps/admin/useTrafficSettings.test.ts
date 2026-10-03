import { ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { saveTraffic } from '../../shared/api/admin'
import type { TrafficInfo } from '../../shared/api/admin'
import { ApiError } from '../../shared/api/client'
import { useTrafficSettings } from './useTrafficSettings'

vi.mock('../../shared/api/admin', () => ({ saveTraffic: vi.fn() }))

function trafficInfo(): TrafficInfo {
  return {
    settings: {
      total: { enabled: true, upload: 4096, download: 8192 },
      guest: { enabled: false, upload: 0, download: 0 },
      users_total: { enabled: true, upload: 2048, download: 4096 },
      users: { alice: { enabled: true, upload: 123, download: 456 } },
      cycle: { unit: 'months', every: 1, anchor: 1704079545, offset_minutes: -300 },
    },
    total: { upload: 0, download: 0 }, guest: { upload: 0, download: 0 },
    users_total: { upload: 0, download: 0 }, users: {}, next_reset: 1790812800, today: '2026-09-02', days: {},
  }
}

function editor(initial: TrafficInfo | undefined = trafficInfo()) {
  const context = {
    info: ref<TrafficInfo | undefined>(initial), refresh: vi.fn<() => Promise<void>>().mockResolvedValue(undefined),
    feedback: vi.fn(), onSaved: vi.fn(),
  }
  return { context, state: useTrafficSettings(context) }
}

afterEach(() => vi.resetAllMocks())

describe('traffic settings editor', () => {
  it('isolates the draft and excludes unchanged fields, preserving the exact cycle', async () => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { context, state } = editor()
    await state.openEditor()
    const original = structuredClone(trafficInfo().settings)
    state.total.value.upload = 9000
    expect(context.info.value!.settings).toEqual(original)
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({ total: { ...original.total, upload: 9000 } })
    expect(context.info.value!.settings.cycle).toEqual(original.cycle)
    expect(state.editing.value).toBe(false)
    expect(context.onSaved).toHaveBeenCalledOnce()
    expect(context.refresh).toHaveBeenCalledOnce()
  })

  it('does not read or copy per-account allowances', async () => {
    const initial = trafficInfo()
    Object.defineProperty(initial.settings, 'users', { get() { throw new Error('Not editable here') } })
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { state } = editor(initial)
    await state.openEditor()
    state.guest.value.enabled = true
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({ guest: { enabled: true, upload: 0, download: 0 } })
  })

  it('compares quota values, not the order of JSON properties', async () => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const initial = trafficInfo()
    initial.settings.total = { download: 8192, upload: 4096, enabled: true }
    const { state } = editor(initial)
    await state.openEditor()
    state.total.value = { enabled: true, upload: 4096, download: 8192 }
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({})
  })

  it('uses the editor-opening snapshot even if statistics refresh while editing', async () => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { context, state } = editor()
    await state.openEditor()
    context.info.value!.settings.total.upload = 7777
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({})
  })

  it('builds a new local midnight and offset only when cycle controls change', async () => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { state } = editor()
    await state.openEditor()
    state.unit.value = 'days'
    state.every.value = 2
    state.anchor.value = '2026-09-30'
    const date = new Date('2026-09-30T00:00:00')
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({ cycle: {
      unit: 'days', every: 2, anchor: Math.floor(date.getTime() / 1000), offset_minutes: -date.getTimezoneOffset(),
    } })
  })

  it('preserves the stored cycle when controls are changed and then restored', async () => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { state } = editor()
    await state.openEditor()
    const originalDate = state.anchor.value
    state.anchor.value = '2026-09-30'
    state.every.value = 2
    state.anchor.value = originalDate
    state.every.value = 1
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({})
  })

  it.each(['total', 'guest', 'usersQuota'] as const)('submits just the changed %s quota group', async group => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { state } = editor()
    await state.openEditor()
    state[group].value.download = 12345
    await state.save()
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({
      [group === 'usersQuota' ? 'users_total' : group]: { ...state[group].value },
    })
  })

  it.each([-1, 0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1])('rejects invalid quota %s even when disabled', async value => {
    const { context, state } = editor()
    await state.openEditor()
    state.guest.value.upload = value
    await state.save()
    expect(saveTraffic).not.toHaveBeenCalled()
    expect(context.onSaved).not.toHaveBeenCalled()
    expect(context.feedback).toHaveBeenCalledOnce()
    expect(state.editing.value).toBe(true)
    expect(state.saving.value).toBe(false)
  })

  it.each([0, 121, 1.5, Number.NaN])('rejects invalid interval %s', async value => {
    const { context, state } = editor()
    await state.openEditor()
    state.every.value = value
    await state.save()
    expect(saveTraffic).not.toHaveBeenCalled()
    expect(context.feedback).toHaveBeenCalledOnce()
  })

  it('rejects an absent start date', async () => {
    const { context, state } = editor()
    await state.openEditor()
    state.anchor.value = ''
    await state.save()
    expect(saveTraffic).not.toHaveBeenCalled()
    expect(context.feedback).toHaveBeenCalledOnce()
  })

  it.each([1, 120])('accepts interval boundary %s and safe integer quotas', async value => {
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { context, state } = editor()
    await state.openEditor()
    state.every.value = value
    state.total.value.upload = Number.MAX_SAFE_INTEGER
    await state.save()
    expect(saveTraffic).toHaveBeenCalledOnce()
    expect(context.feedback).not.toHaveBeenCalled()
  })

  it('fetches missing statistics before opening and stays closed if they remain unavailable', async () => {
    const { context, state } = editor()
    context.info.value = undefined
    await state.openEditor()
    expect(state.editing.value).toBe(false)
    context.refresh.mockImplementation(async () => { context.info.value = trafficInfo() })
    await state.openEditor()
    expect(state.editing.value).toBe(true)
    expect(context.refresh).toHaveBeenCalledTimes(2)
  })

  it('discards unsaved edits when the editor is reopened', async () => {
    const { state } = editor()
    await state.openEditor()
    state.total.value.upload = 9999
    state.editing.value = false
    await state.openEditor()
    expect(state.total.value.upload).toBe(4096)
    expect(saveTraffic).not.toHaveBeenCalled()
  })

  it('does not stack saves or retain mutable form references in the submitted patch', async () => {
    let finish!: (value: { success: boolean }) => void
    vi.mocked(saveTraffic).mockReturnValue(new Promise(resolve => { finish = resolve }))
    const { context, state } = editor()
    await state.openEditor()
    state.total.value.upload = 9000
    const pending = state.save()
    await Promise.all(Array.from({ length: 20 }, () => state.save()))
    state.total.value.upload = 11111
    expect(saveTraffic).toHaveBeenCalledExactlyOnceWith({ total: { enabled: true, upload: 9000, download: 8192 } })
    expect(state.saving.value).toBe(true)
    finish({ success: true })
    await pending
    expect(state.saving.value).toBe(false)
    expect(context.onSaved).toHaveBeenCalledOnce()
  })

  it('reports an unknown result without automatically repeating the write or closing the editor', async () => {
    const error = new ApiError('Unknown result', 0, 'operation_result_unknown')
    vi.mocked(saveTraffic).mockRejectedValue(error)
    const { context, state } = editor()
    await state.openEditor()
    await state.save()
    expect(saveTraffic).toHaveBeenCalledOnce()
    expect(context.feedback).toHaveBeenCalledExactlyOnceWith(error)
    expect(context.onSaved).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
    expect(state.editing.value).toBe(true)
    expect(state.saving.value).toBe(false)
  })

  it('releases busy state after a confirmed rejection and saves a corrected draft', async () => {
    vi.mocked(saveTraffic).mockRejectedValueOnce(new Error('Denied')).mockResolvedValueOnce({ success: true })
    const { context, state } = editor()
    await state.openEditor()
    await state.save()
    expect(state.saving.value).toBe(false)
    expect(state.editing.value).toBe(true)
    state.total.value.upload = 9000
    await state.save()
    expect(saveTraffic).toHaveBeenCalledTimes(2)
    expect(context.onSaved).toHaveBeenCalledOnce()
    expect(state.editing.value).toBe(false)
  })

  it('reports a failed post-save refresh without undoing a confirmed save', async () => {
    const error = new Error('Refresh unavailable')
    vi.mocked(saveTraffic).mockResolvedValue({ success: true })
    const { context, state } = editor()
    await state.openEditor()
    context.refresh.mockRejectedValueOnce(error)
    await state.save()
    expect(context.onSaved).toHaveBeenCalledOnce()
    expect(context.feedback).toHaveBeenCalledExactlyOnceWith(error)
    expect(state.editing.value).toBe(false)
    expect(state.saving.value).toBe(false)
  })
})

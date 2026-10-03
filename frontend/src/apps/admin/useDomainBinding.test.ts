import { effectScope, nextTick, ref, type EffectScope } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '../../shared/api/client'
import { getDomainBinding, removeDomainBinding, saveDomainBinding, type DomainBindingView } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { useDomainBinding } from './useDomainBinding'

vi.mock('../../shared/api/admin', () => ({ getDomainBinding: vi.fn(), removeDomainBinding: vi.fn(), saveDomainBinding: vi.fn() }))
const read = vi.mocked(getDomainBinding)
const save = vi.mocked(saveDomainBinding)
const remove = vi.mocked(removeDomainBinding)
const empty: DomainBindingView = { binding: null, source: 'none' }
const bound: DomainBindingView = { binding: { public_url: 'https://cloud.example.com' }, source: 'settings' }
const scopes: EffectScope[] = []

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup(initial?: DomainBindingView) {
  const current = ref(initial)
  const scope = effectScope()
  scopes.push(scope)
  const editor = scope.run(() => useDomainBinding(() => current.value))!
  return { editor, current, scope }
}

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop()
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('domain binding editor', () => {
  it('starts closed without requesting configuration and isolates the initial binding', () => {
    const initial = { ...bound, binding: { ...bound.binding! } }
    const { editor } = setup(initial)
    const other = setup().editor
    initial.binding.public_url = 'https://external-change.example.com'
    expect(editor.status.value).toEqual(bound)
    expect(other.status.value).toEqual(empty)
    expect(editor.open.value).toBe(false)
    expect(editor.busy.value).toBe(false)
    expect(editor.url.value).toBe('')
    expect(editor.error.value).toBe('')
    expect(editor.message.value).toBe('')
    expect(read).not.toHaveBeenCalled()
    expect(save).not.toHaveBeenCalled()
    expect(remove).not.toHaveBeenCalled()
  })

  it('copies new initial information while idle without changing an unsaved draft', async () => {
    const { editor, current } = setup()
    editor.url.value = 'https://draft.example.com'
    current.value = bound
    await nextTick()
    expect(editor.status.value).toEqual(bound)
    expect(editor.url.value).toBe('https://draft.example.com')
    current.value = undefined
    await nextTick()
    expect(editor.status.value).toEqual(bound)
    current.value = empty
    await nextTick()
    expect(editor.status.value).toEqual(empty)
    expect(read).not.toHaveBeenCalled()
  })

  it('reads fresh configuration only when opening and fills the draft from the response', async () => {
    read.mockResolvedValue(bound)
    const { editor } = setup()
    editor.error.value = 'old error'
    editor.message.value = 'old success'
    await editor.show()
    expect(read).toHaveBeenCalledOnce()
    expect(read.mock.calls[0]![0]).toBeInstanceOf(AbortSignal)
    expect(editor.open.value).toBe(true)
    expect(editor.busy.value).toBe(false)
    expect(editor.status.value).toEqual(bound)
    expect(editor.url.value).toBe(bound.binding!.public_url)
    expect(editor.error.value).toBe('')
    expect(editor.message.value).toBe('')
  })

  it('keeps a failed read separate from known configuration and does not invent a successful read', async () => {
    read.mockRejectedValue(new Error('Configuration unavailable'))
    const { editor } = setup(bound)
    await editor.show()
    expect(editor.status.value).toEqual(bound)
    expect(editor.url.value).toBe(bound.binding!.public_url)
    expect(editor.error.value).toBe('Configuration unavailable')
    expect(editor.message.value).toBe('')
    expect(editor.busy.value).toBe(false)
    // Preserve the existing ability to explicitly save despite a failed read.
    save.mockResolvedValue(bound)
    editor.url.value = ' https://CLOUD.example.com:443/ '
    await editor.submit()
    expect(save).toHaveBeenCalledWith({ public_url: 'https://CLOUD.example.com:443/' })
    expect(editor.error.value).toBe('')
    expect(editor.message.value).toContain('域名绑定已立即生效')
  })

  it('serializes opening with all editor actions and lets the read response win over props received while busy', async () => {
    const pending = deferred<DomainBindingView>()
    read.mockReturnValue(pending.promise)
    const { editor, current } = setup(bound)
    const opening = editor.show()
    await editor.show()
    await editor.submit()
    editor.url.value = ''
    editor.cancel()
    current.value = empty
    await nextTick()
    expect(editor.status.value).toEqual(bound)
    expect(editor.open.value).toBe(true)
    expect(editor.busy.value).toBe(true)
    expect(read).toHaveBeenCalledOnce()
    expect(save).not.toHaveBeenCalled()
    expect(remove).not.toHaveBeenCalled()
    pending.resolve(empty)
    await opening
    expect(editor.status.value).toEqual(empty)
    expect(editor.url.value).toBe('')
    expect(editor.busy.value).toBe(false)
  })

  it('discards cancelled URL and empty drafts without a write', async () => {
    read.mockResolvedValue(bound)
    const { editor } = setup(bound)
    await editor.show()
    editor.url.value = 'https://other.example.com'
    editor.url.value = ''
    editor.error.value = 'old error'
    editor.message.value = 'old success'
    editor.cancel()
    expect(editor.open.value).toBe(false)
    expect(editor.url.value).toBe(bound.binding!.public_url)
    expect(editor.error.value).toBe('')
    expect(editor.message.value).toBe('')
    expect(save).not.toHaveBeenCalled()
    expect(remove).not.toHaveBeenCalled()
    await editor.show()
    expect(read).toHaveBeenCalledTimes(2)
  })

  it('uses the server-normalized binding and isolates response data instead of displaying the sent draft', async () => {
    const result = { ...bound, binding: { ...bound.binding! } }
    read.mockResolvedValue(empty)
    save.mockResolvedValue(result)
    const { editor } = setup()
    await editor.show()
    editor.url.value = '  https://CLOUD.example.com:443/  '
    await editor.submit()
    expect(save).toHaveBeenCalledWith({ public_url: 'https://CLOUD.example.com:443/' })
    expect(editor.status.value).toEqual(bound)
    expect(editor.url.value).toBe(bound.binding!.public_url)
    expect(editor.message.value).toContain('域名绑定已立即生效')
    result.binding.public_url = 'https://external-change.example.com'
    expect(editor.status.value).toEqual(bound)
    expect(editor.url.value).toBe(bound.binding!.public_url)
  })

  it('does not queue repeated writes or switch to removal while saving', async () => {
    const pending = deferred<DomainBindingView>()
    read.mockResolvedValue(bound)
    save.mockReturnValue(pending.promise)
    const { editor } = setup(bound)
    await editor.show()
    editor.url.value = ' https://cloud.example.com '
    const saving = editor.submit()
    editor.url.value = 'https://later-draft.example.com'
    await editor.submit()
    await editor.show()
    editor.url.value = ''
    editor.cancel()
    expect(save).toHaveBeenCalledOnce()
    expect(save).toHaveBeenCalledWith({ public_url: 'https://cloud.example.com' })
    expect(read).toHaveBeenCalledOnce()
    expect(remove).not.toHaveBeenCalled()
    expect(editor.open.value).toBe(true)
    expect(editor.busy.value).toBe(true)
    expect(editor.message.value).toBe('')
    pending.resolve(bound)
    await saving
    expect(editor.url.value).toBe(bound.binding!.public_url)
    expect(editor.busy.value).toBe(false)
  })

  it('requires an open editor before submitting an empty address', async () => {
    const { editor } = setup(bound)
    editor.url.value = ''
    await editor.submit()
    expect(save).not.toHaveBeenCalled()
    expect(remove).not.toHaveBeenCalled()
    read.mockResolvedValue(empty)
    await editor.show()
    expect(remove).not.toHaveBeenCalled()
    remove.mockResolvedValue(empty)
    await editor.submit()
    expect(remove).toHaveBeenCalledOnce()
    expect(editor.status.value).toEqual(empty)
  })

  it('removes only on explicit confirmation and returns the server HTTP state', async () => {
    const pending = deferred<DomainBindingView>()
    read.mockResolvedValue(bound)
    remove.mockReturnValue(pending.promise)
    const { editor } = setup(bound)
    await editor.show()
    editor.error.value = 'old error'
    editor.message.value = 'old success'
    editor.url.value = '   '
    expect(editor.error.value).toBe('old error')
    expect(editor.message.value).toBe('old success')
    expect(remove).not.toHaveBeenCalled()
    const removing = editor.submit()
    await editor.submit()
    editor.cancel()
    expect(remove).toHaveBeenCalledOnce()
    expect(save).not.toHaveBeenCalled()
    expect(editor.status.value).toEqual(bound)
    expect(editor.message.value).toBe('')
    pending.resolve(empty)
    await removing
    expect(editor.status.value).toEqual(empty)
    expect(editor.url.value).toBe('')
    expect(editor.message.value).toContain('恢复 HTTP 访问模式')
    expect(editor.busy.value).toBe(false)
  })

  it.each(['save', 'remove'] as const)('retains a rejected %s draft and waits for manual retry', async operation => {
    read.mockResolvedValue(bound)
    const request = operation === 'save' ? save : remove
    request.mockRejectedValueOnce(new Error('Persistence rejected')).mockResolvedValueOnce(operation === 'save' ? bound : empty)
    const { editor } = setup(bound)
    await editor.show()
    editor.url.value = operation === 'remove' ? '' : 'https://edited.example.com'
    await editor.submit()
    expect(request).toHaveBeenCalledOnce()
    expect(editor.error.value).toBe('Persistence rejected')
    expect(editor.message.value).toBe('')
    expect(editor.url.value).toBe(operation === 'remove' ? '' : 'https://edited.example.com')
    expect(editor.status.value).toEqual(bound)
    expect(editor.busy.value).toBe(false)
    await editor.submit()
    expect(request).toHaveBeenCalledTimes(2)
    expect(editor.error.value).toBe('')
    expect(editor.message.value).not.toBe('')
  })

  it.each(['save', 'remove'] as const)('does not automatically repeat an unconfirmed %s or claim success', async operation => {
    read.mockResolvedValue(bound)
    const request = operation === 'save' ? save : remove
    request.mockRejectedValue(new ApiError('Verify the operation result', 0, 'operation_result_unknown'))
    const { editor } = setup(bound)
    await editor.show()
    if (operation === 'remove') editor.url.value = ''
    await editor.submit()
    expect(request).toHaveBeenCalledOnce()
    expect(read).toHaveBeenCalledOnce()
    expect(editor.status.value).toEqual(bound)
    expect(editor.error.value).toBe('Verify the operation result')
    expect(editor.message.value).toBe('')
  })

  it.each([
    { operation: 'read', message: 'Unable to load domain settings' },
    { operation: 'save', message: 'Unable to save changes' },
    { operation: 'remove', message: 'Unable to save changes' },
  ])('keeps the existing English fallback for $operation failures', async ({ operation, message }) => {
    useLocale().set('en')
    const { editor } = setup(bound)
    read.mockResolvedValue(bound)
    if (operation === 'read') {
      read.mockRejectedValue('unstructured error')
      await editor.show()
    } else {
      await editor.show()
      if (operation === 'remove') {
        remove.mockRejectedValue('unstructured error')
        editor.url.value = ''
      } else save.mockRejectedValue('unstructured error')
      await editor.submit()
    }
    expect(editor.error.value).toBe(message)
    expect(editor.message.value).toBe('')
  })

  it.each(['resolve', 'reject'] as const)('cancels disposed reads and ignores their late %s', async outcome => {
    const pending = deferred<DomainBindingView>()
    read.mockReturnValue(pending.promise)
    const { editor, scope } = setup()
    const opening = editor.show()
    const signal = read.mock.calls[0]![0]!
    scope.stop()
    expect(signal.aborted).toBe(true)
    if (outcome === 'resolve') pending.resolve(bound)
    else pending.reject(new Error('Late failure'))
    await opening
    expect(editor.status.value).toEqual(empty)
    expect(editor.url.value).toBe('')
    expect(editor.open.value).toBe(false)
    expect(editor.error.value).toBe('')
    expect(editor.message.value).toBe('')
    await editor.show()
    await editor.submit()
    editor.url.value = ''
    expect(read).toHaveBeenCalledOnce()
    expect(save).not.toHaveBeenCalled()
    expect(remove).not.toHaveBeenCalled()
  })

  it.each(['save', 'remove'] as const)('ignores confirmed %s after disposal without issuing a follow-up read', async operation => {
    const pending = deferred<DomainBindingView>()
    read.mockResolvedValue(bound)
    const request = operation === 'save' ? save : remove
    request.mockReturnValue(pending.promise)
    const { editor, scope } = setup(bound)
    await editor.show()
    editor.url.value = operation === 'remove' ? '' : 'https://new.example.com'
    const writing = editor.submit()
    expect(request).toHaveBeenCalledOnce()
    scope.stop()
    pending.resolve(operation === 'save' ? bound : empty)
    await writing
    expect(editor.status.value).toEqual(bound)
    expect(editor.open.value).toBe(false)
    expect(editor.url.value).toBe('')
    expect(editor.message.value).toBe('')
    await editor.submit()
    await editor.show()
    expect(request).toHaveBeenCalledOnce()
    expect(read).toHaveBeenCalledOnce()
  })
})

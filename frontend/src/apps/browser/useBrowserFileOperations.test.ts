import { effectScope, ref, type EffectScope } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { batchOperation, type BatchOperation, type BatchResponse } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useLocale } from '../../shared/i18n'
import { useBrowserFileOperations } from './useBrowserFileOperations'

vi.mock('../../shared/api/browser', async importOriginal => ({
  ...await importOriginal<typeof import('../../shared/api/browser')>(),
  batchOperation: vi.fn(),
}))

const batch = vi.mocked(batchOperation)
const scopes: EffectScope[] = []
const operations = ['delete', 'move', 'copy'] as const
const completed: BatchResponse = { success: 1, failed: 0, results: [
  { path: 'one.txt', status: 200, code: 'ok', message: 'Completed' },
] }

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

function setup() {
  const scope = effectScope()
  scopes.push(scope)
  const context = {
    storageId: ref('primary'), selected: ref(new Set(['one.txt'])),
    requireCapability: vi.fn().mockReturnValue(true), announce: vi.fn(), refresh: vi.fn().mockResolvedValue(undefined),
  }
  const actions = scope.run(() => useBrowserFileOperations(context))!
  const open = (operation: BatchOperation, paths = ['one.txt']) => {
    if (operation === 'delete') actions.requestDelete(paths)
    else actions.requestTransfer(operation, paths)
  }
  const confirm = (operation: BatchOperation) => operation === 'delete' ? actions.confirmDelete() : actions.confirmTransfer('target')
  return { scope, context, actions, open, confirm }
}

beforeEach(() => {
  useLocale().set('zh-CN')
  batch.mockResolvedValue(completed)
})
afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  vi.resetAllMocks()
  useLocale().set('zh-CN')
})

describe('browser batch operation lifecycle', () => {
  it('starts closed and ignores confirmations without a matching dialog', async () => {
    const { actions, context } = setup()
    await actions.confirmDelete()
    await actions.confirmTransfer('target')
    expect(actions.showDelete.value).toBe(false)
    expect(actions.pendingDelete.value).toEqual([])
    expect(actions.pickerOperation.value).toBeNull()
    expect(actions.pickerStorageId.value).toBe('')
    expect(actions.batchResult.value).toBeNull()
    expect(batch).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
  })

  it.each(operations)('requires the matching capability and nonempty paths for %s', operation => {
    const { open, context, actions } = setup()
    open(operation, [])
    expect(context.requireCapability).not.toHaveBeenCalled()
    context.requireCapability.mockReturnValue(false)
    open(operation)
    expect(context.requireCapability).toHaveBeenCalledExactlyOnceWith(operation === 'move' ? 'move_items' : operation)
    expect(actions.showDelete.value).toBe(false)
    expect(actions.pickerOperation.value).toBeNull()
    expect(batch).not.toHaveBeenCalled()
  })

  it('replaces a delete confirmation when a transfer picker is opened', () => {
    const { open, actions } = setup()
    open('delete')
    open('copy', ['two.txt'])
    expect(actions.showDelete.value).toBe(false)
    expect(actions.pendingDelete.value).toEqual([])
    expect(actions.pickerOperation.value).toBe('copy')
    expect(actions.pickerTitle.value).toContain('1 个项目')
  })

  it('replaces a transfer picker with a delete confirmation and clears closed context', () => {
    const { open, actions, context } = setup()
    open('move', ['first.txt'])
    context.storageId.value = 'secondary'
    open('delete', ['second.txt'])
    expect(actions.pickerOperation.value).toBeNull()
    expect(actions.pickerStorageId.value).toBe('')
    expect(actions.showDelete.value).toBe(true)
    expect(actions.pendingDelete.value).toEqual(['second.txt'])
    actions.closeBatchDialog()
    expect(actions.showDelete.value).toBe(false)
    expect(actions.pendingDelete.value).toEqual([])
  })

  it.each(operations)('freezes %s sources and rejects mismatched or repeated confirmations', async operation => {
    const pending = deferred<BatchResponse>()
    batch.mockReturnValue(pending.promise)
    const { open, confirm, actions, context } = setup()
    const paths = ['one.txt']
    open(operation, paths)
    paths.push('unselected.txt')
    context.storageId.value = 'secondary'
    if (operation === 'delete') await actions.confirmTransfer('wrong-target')
    else await actions.confirmDelete()
    expect(batch).not.toHaveBeenCalled()
    const request = confirm(operation)
    await confirm(operation)
    actions.closeBatchDialog()
    open('delete', ['other.txt'])
    open('copy', ['other.txt'])
    expect(batch).toHaveBeenCalledExactlyOnceWith(operation, ['one.txt'], operation === 'delete' ? '' : 'target', 'primary')
    expect(actions.showDelete.value).toBe(operation === 'delete')
    expect(actions.pickerOperation.value).toBeNull()
    pending.resolve(completed)
    await request
    expect(context.selected.value.size).toBe(0)
  })

  it('allows an explicit delete retry after a definite rejection and resets it on close', async () => {
    batch.mockRejectedValueOnce(new ApiError('Denied', 403))
    const { open, actions, context } = setup()
    open('delete')
    await actions.confirmDelete()
    expect(actions.showDelete.value).toBe(true)
    expect(actions.operationError.value).toBe('Denied')
    expect(actions.operationRetryBlocked.value).toBe(false)
    expect(context.refresh).not.toHaveBeenCalled()
    await actions.confirmDelete()
    expect(batch).toHaveBeenCalledTimes(2)
    expect(actions.operationError.value).toBe('')
    expect(actions.showDelete.value).toBe(false)
    open('delete')
    expect(actions.operationRetryBlocked.value).toBe(false)
  })

  it.each([
    new ApiError('Verify first', 503, 'operation_result_unknown'),
    new ApiError('Already committed', 503, 'service_unavailable', undefined, { commit: 'committed', cleanup: 'pending', retry: 'do_not_repeat' }),
  ])('does not reconfirm a delete with unsafe retry evidence: $message', async error => {
    batch.mockRejectedValue(error)
    const { open, actions, context } = setup()
    open('delete')
    await actions.confirmDelete()
    await actions.confirmDelete()
    expect(actions.showDelete.value).toBe(true)
    expect(actions.operationRetryBlocked.value).toBe(true)
    expect(actions.operationError.value).toBe(error.message)
    expect(batch).toHaveBeenCalledOnce()
    expect(context.refresh).not.toHaveBeenCalled()
    actions.closeBatchDialog()
    expect(actions.operationError.value).toBe('')
    expect(actions.operationRetryBlocked.value).toBe(false)
  })

  it.each(['move', 'copy'] as const)('closes a failed %s picker without automatically retrying', async operation => {
    batch.mockRejectedValue(new ApiError('Denied', 403))
    const { open, confirm, actions, context } = setup()
    open(operation)
    await confirm(operation)
    await confirm(operation)
    expect(actions.pickerOperation.value).toBeNull()
    expect(actions.operationError.value).toBe('Denied')
    expect(context.announce).toHaveBeenCalledExactlyOnceWith('Denied')
    expect(context.refresh).not.toHaveBeenCalled()
    expect(batch).toHaveBeenCalledOnce()
    batch.mockResolvedValue(completed)
    open(operation)
    expect(actions.operationError.value).toBe('')
    await confirm(operation)
    expect(batch).toHaveBeenCalledTimes(2)
  })

  it.each(operations)('does not turn a confirmed %s into a write failure when refresh fails', async operation => {
    const { context, actions, open, confirm } = setup()
    context.refresh.mockRejectedValue(new Error('Listing unavailable'))
    open(operation)
    await confirm(operation)
    expect(context.announce).toHaveBeenCalledWith(expect.stringContaining('成功'), 'success')
    expect(context.announce).toHaveBeenLastCalledWith('Listing unavailable')
    expect(actions.operationError.value).toBe('')
    expect(actions.showDelete.value).toBe(false)
    await actions.confirmDelete()
    await actions.confirmTransfer('target')
    expect(batch).toHaveBeenCalledOnce()
  })

  it.each(operations)('releases a confirmed %s before a slow refresh without closing a later dialog', async operation => {
    const pending = deferred<void>()
    const { context, actions, open, confirm } = setup()
    context.refresh.mockReturnValue(pending.promise)
    open(operation)
    const request = confirm(operation)
    await Promise.resolve()
    expect(actions.operationBusy.value).toBe(false)
    expect(actions.showDelete.value).toBe(false)
    expect(context.announce).toHaveBeenCalledWith(expect.stringContaining('成功'), 'success')
    open('delete', ['later.txt'])
    pending.resolve()
    await request
    expect(actions.showDelete.value).toBe(true)
    expect(actions.pendingDelete.value).toEqual(['later.txt'])
    expect(batch).toHaveBeenCalledOnce()
  })

  it.each(operations)('does not refresh, announce or clear selection after an accepted %s outlives the page', async operation => {
    const pending = deferred<BatchResponse>()
    batch.mockReturnValue(pending.promise)
    const { scope, context, actions, open, confirm } = setup()
    open(operation)
    const request = confirm(operation)
    scope.stop()
    pending.resolve(completed)
    await request
    expect(context.announce).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
    expect(context.selected.value).toEqual(new Set(['one.txt']))
    expect(actions.operationBusy.value).toBe(false)
    expect(actions.batchResult.value).toBeNull()
    open('delete')
    await actions.confirmDelete()
    expect(batch).toHaveBeenCalledOnce()
  })

  it.each(operations)('ignores a rejected %s after leaving the page and clears local state', async operation => {
    const pending = deferred<BatchResponse>()
    batch.mockReturnValue(pending.promise)
    const { scope, open, confirm, actions, context } = setup()
    open(operation)
    const request = confirm(operation)
    scope.stop()
    pending.reject(new ApiError('Verify first', 503, 'operation_result_unknown'))
    await request
    expect(actions.showDelete.value).toBe(false)
    expect(actions.pickerOperation.value).toBeNull()
    expect(actions.pendingDelete.value).toEqual([])
    expect(actions.operationError.value).toBe('')
    expect(actions.operationRetryBlocked.value).toBe(false)
    expect(actions.operationBusy.value).toBe(false)
    expect(context.announce).not.toHaveBeenCalled()
    expect(context.refresh).not.toHaveBeenCalled()
    open(operation)
    await confirm(operation)
    expect(batch).toHaveBeenCalledOnce()
  })

  it('ignores a refresh failure after disposal without reopening or repeating a confirmed delete', async () => {
    const pending = deferred<void>()
    const { context, scope, open, actions } = setup()
    context.refresh.mockReturnValue(pending.promise)
    open('delete')
    const request = actions.confirmDelete()
    await Promise.resolve()
    expect(context.announce).toHaveBeenCalledOnce()
    scope.stop()
    pending.reject(new Error('Listing unavailable'))
    await request
    expect(context.announce).toHaveBeenCalledOnce()
    expect(actions.showDelete.value).toBe(false)
    expect(actions.operationError.value).toBe('')
    expect(batch).toHaveBeenCalledOnce()
  })

  it.each(operations)('retains English %s success text and localized fallback errors', async operation => {
    useLocale().set('en')
    const { open, confirm, context } = setup()
    open(operation)
    await confirm(operation)
    const label = operation === 'delete' ? 'Delete' : operation === 'move' ? 'Move' : 'Copy'
    expect(context.announce).toHaveBeenCalledWith(`${label} completed`, 'success')
    batch.mockRejectedValue(null)
    open(operation)
    await confirm(operation)
    expect(context.announce).toHaveBeenLastCalledWith(useLocale().t('common.failed'))
  })
})

import { describe, expect, it, vi } from 'vitest'
import type { UploadBatchItemState } from '../../shared/api/browser'
import type { OperationOutcome } from '../../shared/api/client'
import type { UploadTask } from './uploadQueue'
import { applyUploadServerState, markUploadUnconfirmed } from './uploadTaskState'

function task(): UploadTask {
  return { id: 1, file: new File(['data'], 'file.txt'), storageId: 'first', basePath: 'folder',
    relativePath: 'file (1).txt', targetPath: 'folder/file (1).txt', originalTargetPath: 'folder/file.txt',
    ticket: 'ticket', directUpload: true, attempted: true, status: 'verifying', loaded: 3,
    error: 'old error', retryBlocked: true, safeToPrepare: false }
}

const chinese = (zh: string) => zh

describe('upload server state interpretation', () => {
  it.each(['pending', 'failed'] as const)('allows a confirmed %s result to be retried', status => {
    const current = task()
    applyUploadServerState(current, status, undefined, chinese)
    expect(current).toMatchObject({ status: 'failed', loaded: 0, retryBlocked: false, safeToPrepare: true,
      cancelRequested: false, pauseRequested: false })
    expect(current.error).toContain(status === 'pending' ? '尚未开始' : '上传失败')
  })

  it.each(['pending', 'failed'] as const)('settles a pause only after a %s result', status => {
    const current = { ...task(), pauseRequested: true }
    applyUploadServerState(current, status, undefined, chinese)
    expect(current).toMatchObject({ status: 'paused', loaded: 0, error: '', retryBlocked: false,
      safeToPrepare: true, pauseRequested: false })
  })

  it('gives termination priority over pause after a confirmed failure', () => {
    const current = { ...task(), cancelRequested: true, pauseRequested: true }
    applyUploadServerState(current, 'failed', undefined, chinese)
    expect(current).toMatchObject({ status: 'cancelled', loaded: 0, safeToPrepare: true,
      retryBlocked: false, cancelRequested: false, pauseRequested: false })
    expect(current.error).toContain('确认未提交')
  })

  it.each([undefined, { commit: 'not_committed', cleanup: 'complete', retry: 'after_correction' } as OperationOutcome])('settles a confirmed cancellation with outcome %j', operation => {
    const current = { ...task(), cancelRequested: true, pauseRequested: true }
    applyUploadServerState(current, 'cancelled', operation, chinese)
    expect(current).toMatchObject({ status: 'cancelled', loaded: 0, safeToPrepare: true,
      retryBlocked: false, cancelRequested: false, pauseRequested: false })
    expect(current.error).toContain('确认未提交')
  })

  it('does not turn an administrator-closed uncertain result into proof of non-commit', () => {
    const current = { ...task(), cancelRequested: true, pauseRequested: true }
    applyUploadServerState(current, 'cancelled', { commit: 'unknown', cleanup: 'unknown', retry: 'verify_first' }, chinese)
    expect(current).toMatchObject({ status: 'cancelled', safeToPrepare: false, retryBlocked: true,
      cancelRequested: false, pauseRequested: false })
    expect(current.error).not.toContain('确认未提交')
  })

  it.each([undefined, 'complete', 'pending', 'unknown'] as const)('preserves confirmed success independently of cleanup %s', cleanup => {
    const current = { ...task(), cancelRequested: true, pauseRequested: true }
    const operation: OperationOutcome | undefined = cleanup === undefined ? undefined
      : { commit: 'committed', cleanup, retry: 'do_not_repeat' }
    applyUploadServerState(current, 'complete', operation, chinese)
    expect(current).toMatchObject({ status: 'succeeded', loaded: current.file.size, retryBlocked: false,
      safeToPrepare: false, cancelRequested: false, pauseRequested: false })
    expect(current.error).toBe(cleanup === 'complete' ? '' : '上传成功，临时数据仍在后台清理')
  })

  describe.each(['unknown', 'in_progress'] as const)('%s evidence', status => {
    it.each([[false, false], [true, false], [false, true], [true, true]])('keeps retry blocked with cancel=%s and pause=%s', (cancelRequested, pauseRequested) => {
      const current = { ...task(), cancelRequested, pauseRequested }
      applyUploadServerState(current, status, undefined, chinese)
      expect(current).toMatchObject({ status: 'verifying', loaded: 3, retryBlocked: true, safeToPrepare: false,
        cancelRequested, pauseRequested })
      expect(current.error).not.toContain('可以重试')
    })
  })

  it('marks an unconfirmed result without inventing cancellation or success', () => {
    const current = { ...task(), cancelRequested: true, pauseRequested: false }
    const text = vi.fn(chinese)
    markUploadUnconfirmed(current, text)
    expect(current).toMatchObject({ status: 'verifying', loaded: 3, retryBlocked: true,
      cancelRequested: true, pauseRequested: false })
    expect(text).toHaveBeenCalledOnce()
  })

  it.each(['pending', 'failed', 'complete', 'unknown', 'in_progress', 'cancelled'] as UploadBatchItemState[])('does not change operation identity or input after %s', status => {
    const current = task()
    const bindings = { id: current.id, file: current.file, storageId: current.storageId, basePath: current.basePath,
      targetPath: current.targetPath, originalTargetPath: current.originalTargetPath, relativePath: current.relativePath,
      ticket: current.ticket, directUpload: current.directUpload, attempted: current.attempted }
    applyUploadServerState(current, status, undefined, chinese)
    expect(current).toMatchObject(bindings)
    expect(current.file).toBe(bindings.file)
  })
})

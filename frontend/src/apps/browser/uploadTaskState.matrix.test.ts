import { describe, expect, it } from 'vitest'
import type { OperationOutcome } from '../../shared/api/client'
import type { UploadTask } from './uploadQueue'
import { applyUploadServerState } from './uploadTaskState'

function task(overrides: Partial<UploadTask> = {}): UploadTask {
  return {
    id: 1,
    file: new File(['data'], 'file.txt'),
    storageId: 'first',
    basePath: 'folder',
    relativePath: 'file.txt',
    targetPath: 'folder/file.txt',
    originalTargetPath: 'folder/file.txt',
    ticket: 'ticket',
    directUpload: true,
    attempted: true,
    status: 'uploading',
    loaded: 0,
    error: '',
    retryBlocked: false,
    safeToPrepare: false,
    ...overrides,
  }
}

const zh = (text: string) => text
const outcome = (value: Record<string, string>): OperationOutcome => value as unknown as OperationOutcome

describe('upload state matrix', () => {
  it('keeps a cancel that lost the race against completion', () => {
    const current = task({ status: 'verifying', cancelRequested: true })
    applyUploadServerState(current, 'complete', outcome({ commit: 'committed', cleanup: 'complete' }), zh)
    expect(current.status).toBe('succeeded')
    expect(current.cancelRequested).toBe(false)
    expect(current.pauseRequested).toBe(false)
  })

  it('blocks retry when the server reports cancelled with an unknown commit', () => {
    const current = task({ status: 'verifying', cancelRequested: true })
    applyUploadServerState(current, 'cancelled', outcome({ commit: 'unknown' }), zh)
    expect(current.status).toBe('cancelled')
    expect(current.retryBlocked).toBe(true)
    expect(current.safeToPrepare).toBe(false)
    expect(current.error).toContain('确认上传结果')
  })

  it('turns a failed result into cancelled when a cancel was requested', () => {
    const current = task({ status: 'verifying', cancelRequested: true })
    applyUploadServerState(current, 'failed', undefined, zh)
    expect(current.status).toBe('cancelled')
    expect(current.loaded).toBe(0)
    expect(current.cancelRequested).toBe(false)
  })

  it('keeps verifying while the server still reports in_progress', () => {
    const current = task({ status: 'uploading' })
    applyUploadServerState(current, 'in_progress', undefined, zh)
    expect(current.status).toBe('verifying')
    expect(current.retryBlocked).toBe(true)
    expect(current.error).toContain('仍在服务端执行')
  })
})

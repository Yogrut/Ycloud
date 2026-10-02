import { describe, expect, it } from 'vitest'
import {
  canUploadTaskAction, isUploadPathReserved, isUploadTaskActive,
  summarizeUploadTasks, uploadLoadedBytes, uploadTaskPercent,
} from './uploadQueue'
import type { UploadTask, UploadTaskAction, UploadTaskStatus } from './uploadQueue'

function task(status: UploadTaskStatus, loaded = 4, size = 4): UploadTask {
  return {
    id: 1, storageId: 'primary', basePath: '', targetPath: 'file.bin', relativePath: 'file.bin',
    file: new File([new Uint8Array(size)], 'file.bin'), status, loaded, error: '',
  }
}

const actions: UploadTaskAction[] = ['pause', 'resume', 'terminate', 'retry', 'clear']
const eligibility: Array<[UploadTaskStatus, UploadTaskAction[], boolean]> = [
  ['preparing', ['pause', 'terminate'], true],
  ['queued', ['pause', 'terminate'], true],
  ['uploading', ['pause', 'terminate'], true],
  ['paused', ['resume', 'terminate'], true],
  ['verifying', [], true],
  ['succeeded', ['clear'], false],
  ['failed', ['retry', 'clear'], false],
  ['cancelled', ['clear'], false],
]

describe('upload task rules', () => {
  it.each(eligibility)('defines actions and activity for %s', (status, allowed, active) => {
    const item = task(status)
    expect(actions.filter(action => canUploadTaskAction(item, action))).toEqual(allowed)
    expect(isUploadTaskActive(item)).toBe(active)
    expect(isUploadPathReserved(item)).toBe(active)
    expect(uploadTaskPercent(item)).toBe(status === 'succeeded' ? 100
      : status === 'uploading' || status === 'verifying' ? 99 : 0)
  })

  it('blocks uncertain retries and reserves their paths without preventing record removal', () => {
    for (const status of ['failed', 'cancelled'] as const) {
      const item = { ...task(status), retryBlocked: true }
      expect(canUploadTaskAction(item, 'retry')).toBe(false)
      expect(canUploadTaskAction(item, 'clear')).toBe(true)
      expect(isUploadPathReserved(item)).toBe(true)
      expect(item.status).toBe(status)
    }
  })

  it.each([
    [-10, 0, 0], [2, 2, 50], [10, 4, 99], [Number.NaN, 0, 0],
  ])('bounds loaded bytes %s without implying success', (loaded, bytes, percent) => {
    const item = task('uploading', loaded)
    expect(uploadLoadedBytes(item)).toBe(bytes)
    expect(uploadTaskPercent(item)).toBe(percent)
    expect(summarizeUploadTasks([item]).percent).toBe(percent)
    expect(item.loaded).toBe(loaded)
  })

  it('counts mixed statuses once and excludes failed, paused and cancelled transfer bytes', () => {
    const items = eligibility.map(([status]) => task(status, 2))
    expect(summarizeUploadTasks(items)).toEqual({
      succeeded: 1, failed: 1, cancelled: 1, active: 5,
      totalBytes: 32, uploadedBytes: 8, percent: 25,
    })
    expect(items.every(item => item.loaded === 2)).toBe(true)
  })

  it('requires every task to succeed before showing a complete queue', () => {
    expect(summarizeUploadTasks([task('succeeded'), task('verifying')]).percent).toBe(99)
    expect(summarizeUploadTasks([task('succeeded'), task('failed', 0, 0)]).percent).toBe(99)
    expect(summarizeUploadTasks([task('succeeded', 0), task('succeeded')]).percent).toBe(100)
  })

  it('handles empty queues and zero-byte files without division by zero', () => {
    expect(summarizeUploadTasks([])).toEqual({
      succeeded: 0, failed: 0, cancelled: 0, active: 0,
      totalBytes: 0, uploadedBytes: 0, percent: 0,
    })
    for (const status of ['queued', 'uploading', 'verifying', 'succeeded'] as const) {
      const item = task(status, 0, 0)
      expect(uploadTaskPercent(item)).toBe(status === 'succeeded' ? 100 : 0)
      expect(summarizeUploadTasks([item]).percent).toBe(status === 'succeeded' ? 100 : 0)
    }
  })
})

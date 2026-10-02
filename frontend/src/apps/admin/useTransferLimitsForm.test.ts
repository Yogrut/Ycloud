import { afterEach, describe, expect, it } from 'vitest'
import { useLocale } from '../../shared/i18n'
import { GIB, formatGiB, useTransferLimitsForm, type TransferLimitsInfo } from './useTransferLimitsForm'

const MIB = 1024 ** 2

function info(overrides: Partial<TransferLimitsInfo> = {}): TransferLimitsInfo {
  return {
    max_upload_bytes: 5 * GIB, max_upload_batch_bytes: 20 * GIB, max_upload_batch_entries: 1000,
    max_archive_bytes: 3 * GIB, max_archive_entries: 1000,
    upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: 0,
    ...overrides,
  }
}

afterEach(() => useLocale().set('zh-CN'))

describe('transfer limits draft', () => {
  it('loads readable inputs while exposing only editable fields', () => {
    const current = info()
    Object.defineProperty(current, 'user_accounts', { get: () => { throw new Error('unrelated account data read') } })
    const form = useTransferLimitsForm(() => current)
    expect(form.initial).toEqual({ uploadGiB: '5', uploadBatchGiB: '20', uploadBatchEntries: '1000', archiveGiB: '3', archiveEntries: '1000', uploadRate: '0', downloadRate: '0' })
    expect(form.hasChanges.value).toBe(false)
    expect(form.buildRequest()).toEqual(current)
    expect(form.changes(form.buildRequest())).toEqual({})
  })

  it('uses existing batch defaults for older responses and preserves their request behavior', () => {
    const current = info({ max_upload_batch_bytes: undefined, max_upload_batch_entries: undefined })
    const form = useTransferLimitsForm(() => current)
    expect(form.uploadBatchGiB.value).toBe('20')
    expect(form.uploadBatchEntries.value).toBe('1000')
    expect(form.changes(form.buildRequest())).toEqual({ max_upload_batch_bytes: 20 * GIB, max_upload_batch_entries: 1000 })
  })

  it('preserves unedited exact bytes and rates, including after editing and reverting their inputs', () => {
    const current = info({ max_upload_bytes: MIB, max_upload_batch_bytes: 20 * GIB + 123, max_archive_bytes: 3 * GIB + 234, upload_rate_bytes_per_sec: MIB + 1, download_rate_bytes_per_sec: 2 * MIB + 3 })
    const form = useTransferLimitsForm(() => current)
    expect(form.uploadGiB.value).toBe('0.001')
    expect(form.uploadRate.value).toBe('1')
    form.uploadRate.value = 2
    expect(form.hasChanges.value).toBe(true)
    form.uploadRate.value = '1'
    expect(form.hasChanges.value).toBe(false)
    form.archiveEntries.value = 999
    expect(form.buildRequest()).toEqual({ ...current, max_archive_entries: 999 })
    expect(form.changes(form.buildRequest())).toEqual({ max_archive_entries: 999 })
  })

  it('allows whitespace around unedited inputs without changing the precision baseline', () => {
    const current = info({ upload_rate_bytes_per_sec: MIB + 1 })
    const form = useTransferLimitsForm(() => current)
    form.uploadGiB.value = ' 5 '
    form.uploadRate.value = ' 1 '
    expect(form.hasChanges.value).toBe(false)
    expect(form.buildRequest()).toEqual(current)
  })

  it('resets to the current info without replacing draft references and does not share drafts', () => {
    let current = info()
    const form = useTransferLimitsForm(() => current)
    const other = useTransferLimitsForm(() => current)
    const upload = form.uploadGiB
    form.uploadGiB.value = 6
    expect(other.uploadGiB.value).toBe('5')
    current = info({ max_upload_bytes: 7 * GIB })
    form.reset()
    expect(form.uploadGiB).toBe(upload)
    expect(form.uploadGiB.value).toBe('7')
    expect(form.hasChanges.value).toBe(false)
  })

  it('accepts a successful candidate through the same reset path and copies its exact baseline', () => {
    const form = useTransferLimitsForm(() => info())
    const candidate = { ...info(), max_upload_batch_bytes: 20 * GIB, max_upload_batch_entries: 1000, upload_rate_bytes_per_sec: MIB + 7 }
    form.accept(candidate)
    candidate.upload_rate_bytes_per_sec = 99
    expect(form.uploadRate.value).toBe('1')
    expect(form.hasChanges.value).toBe(false)
    expect(form.buildRequest().upload_rate_bytes_per_sec).toBe(MIB + 7)
  })

  it.each([
    ['uploadGiB', 6, 'max_upload_bytes', 6 * GIB],
    ['uploadBatchGiB', 21, 'max_upload_batch_bytes', 21 * GIB],
    ['uploadBatchEntries', 999, 'max_upload_batch_entries', 999],
    ['archiveGiB', 4, 'max_archive_bytes', 4 * GIB],
    ['archiveEntries', 999, 'max_archive_entries', 999],
    ['uploadRate', 1, 'upload_rate_bytes_per_sec', MIB],
    ['downloadRate', 2, 'download_rate_bytes_per_sec', 2 * MIB],
  ] as const)('submits only the changed %s field', (input, value, field, expected) => {
    const form = useTransferLimitsForm(() => info())
    form[input].value = value
    const candidate = form.buildRequest()
    expect(form.changes(candidate)).toEqual({ [field]: expected })
    form[input].value = '0'
    expect(candidate[field]).toBe(expected)
  })

  it('does not compare against unrelated info fields or depend on object insertion order', () => {
    const current = info()
    const form = useTransferLimitsForm(() => current)
    const candidate = form.buildRequest()
    expect(form.changes({ download_rate_bytes_per_sec: candidate.download_rate_bytes_per_sec, upload_rate_bytes_per_sec: candidate.upload_rate_bytes_per_sec, max_archive_entries: candidate.max_archive_entries, max_archive_bytes: candidate.max_archive_bytes, max_upload_batch_entries: candidate.max_upload_batch_entries, max_upload_batch_bytes: candidate.max_upload_batch_bytes, max_upload_bytes: candidate.max_upload_bytes })).toEqual({})
  })
})

describe('transfer limits boundaries', () => {
  it.each([MIB / GIB, 100])('accepts changed byte-size boundaries: %s GiB', value => {
    const form = useTransferLimitsForm(() => info())
    form.uploadGiB.value = value
    form.uploadBatchGiB.value = value
    form.archiveGiB.value = value
    const candidate = form.buildRequest()
    expect(candidate.max_upload_bytes).toBe(Math.round(value * GIB))
    expect(candidate.max_upload_batch_bytes).toBe(candidate.max_upload_bytes)
    expect(candidate.max_archive_bytes).toBe(candidate.max_upload_bytes)
  })

  it.each([0, -1, NaN, Infinity, MIB / GIB / 2, 101])('rejects changed invalid byte-size inputs: %s', value => {
    for (const input of ['uploadGiB', 'uploadBatchGiB', 'archiveGiB'] as const) {
      const form = useTransferLimitsForm(() => info())
      form[input].value = value
      expect(() => form.buildRequest()).toThrow()
    }
  })

  it('uses declared deployment ceilings and enforces batch size against the single-file limit', () => {
    const form = useTransferLimitsForm(() => info({ deployment_max_upload_bytes: 6 * GIB, deployment_max_upload_batch_bytes: 21 * GIB, deployment_max_archive_bytes: 4 * GIB }))
    form.uploadGiB.value = 7
    expect(() => form.buildRequest()).toThrow('6 GiB')
    form.uploadGiB.value = 6
    form.uploadBatchGiB.value = 22
    expect(() => form.buildRequest()).toThrow('21 GiB')
    form.uploadBatchGiB.value = 5
    expect(() => form.buildRequest()).toThrow('不能小于单文件')
    form.uploadBatchGiB.value = 21
    form.archiveGiB.value = 5
    expect(() => form.buildRequest()).toThrow('4 GiB')
    form.archiveGiB.value = 4
    expect(form.buildRequest().max_archive_bytes).toBe(4 * GIB)
  })

  it.each([0, -1, 0.5, NaN, Infinity, 100_001])('rejects invalid entry counts: %s', value => {
    for (const input of ['uploadBatchEntries', 'archiveEntries'] as const) {
      const form = useTransferLimitsForm(() => info())
      form[input].value = value
      expect(() => form.buildRequest()).toThrow()
    }
  })

  it('accepts declared entry ceilings and rejects exceeding them', () => {
    const form = useTransferLimitsForm(() => info({ deployment_max_upload_batch_entries: 1200, deployment_max_archive_entries: 1500 }))
    form.uploadBatchEntries.value = 1200
    form.archiveEntries.value = 1500
    expect(form.buildRequest()).toMatchObject({ max_upload_batch_entries: 1200, max_archive_entries: 1500 })
    form.uploadBatchEntries.value = 1201
    expect(() => form.buildRequest()).toThrow('1200')
    form.uploadBatchEntries.value = 1200
    form.archiveEntries.value = 1501
    expect(() => form.buildRequest()).toThrow('1500')
  })

  it.each([0, 0.0625, 1024])('accepts rate boundaries: %s MiB/s', value => {
    const form = useTransferLimitsForm(() => info({ upload_rate_bytes_per_sec: MIB, download_rate_bytes_per_sec: MIB }))
    form.uploadRate.value = value
    form.downloadRate.value = value
    expect(form.buildRequest()).toMatchObject({ upload_rate_bytes_per_sec: value * MIB, download_rate_bytes_per_sec: value * MIB })
  })

  it.each([-1, 0.03125, 1025, NaN, Infinity])('rejects invalid rates: %s MiB/s', value => {
    for (const input of ['uploadRate', 'downloadRate'] as const) {
      const form = useTransferLimitsForm(() => info())
      form[input].value = value
      expect(() => form.buildRequest()).toThrow()
    }
  })

  it('rounds explicitly edited fractional rates to integer bytes and treats a cleared rate as unlimited', () => {
    const form = useTransferLimitsForm(() => info({ upload_rate_bytes_per_sec: MIB }))
    form.uploadRate.value = ''
    form.downloadRate.value = 0.123456
    expect(form.buildRequest()).toMatchObject({ upload_rate_bytes_per_sec: 0, download_rate_bytes_per_sec: Math.round(0.123456 * MIB) })
  })

  it('uses existing bilingual validation and display precision', () => {
    expect(formatGiB(MIB)).toBe('0.001')
    useLocale().set('en')
    const form = useTransferLimitsForm(() => info())
    form.uploadRate.value = 0.001
    expect(() => form.buildRequest()).toThrow('Upload rate limit must be 0')
  })
})

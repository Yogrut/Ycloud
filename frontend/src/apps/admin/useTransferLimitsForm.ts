import { computed, reactive, toRefs } from 'vue'
import type { AdminInfo, UpdateTransferLimitsRequest } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

export const GIB = 1024 ** 3
const MIB = 1024 ** 2
const DEFAULT_BATCH_BYTES = 20 * GIB
const DEFAULT_BATCH_ENTRIES = 1000
export const DEFAULT_MAX_BYTES = 100 * GIB
export const DEFAULT_MAX_BATCH_ENTRIES = 10_000
export const DEFAULT_MAX_ARCHIVE_ENTRIES = 100_000
const HARD_MAX_RATE_BYTES = 1024 * MIB
const MIN_RATE_BYTES = 64 * 1024

const LIMIT_FIELDS = [
  'max_upload_bytes', 'max_upload_batch_bytes', 'max_upload_batch_entries',
  'max_archive_bytes', 'max_archive_entries', 'upload_rate_bytes_per_sec', 'download_rate_bytes_per_sec',
] as const

export type TransferLimitsInfo = Pick<AdminInfo, keyof UpdateTransferLimitsRequest
  | 'deployment_max_upload_bytes' | 'deployment_max_upload_batch_bytes' | 'deployment_max_upload_batch_entries'
  | 'deployment_max_archive_bytes' | 'deployment_max_archive_entries'>

interface TransferLimitsInputs {
  uploadGiB: string | number
  uploadBatchGiB: string | number
  uploadBatchEntries: string | number
  archiveGiB: string | number
  archiveEntries: string | number
  uploadRate: string | number
  downloadRate: string | number
}

export function formatGiB(bytes: number): string {
  return Number((bytes / GIB).toFixed(3)).toString()
}

function formatRate(bytes: number): string {
  return bytes === 0 ? '0' : Number((bytes / MIB).toFixed(4)).toString()
}

function currentLimits(info: TransferLimitsInfo): UpdateTransferLimitsRequest {
  return {
    max_upload_bytes: info.max_upload_bytes,
    max_upload_batch_bytes: info.max_upload_batch_bytes ?? DEFAULT_BATCH_BYTES,
    max_upload_batch_entries: info.max_upload_batch_entries ?? DEFAULT_BATCH_ENTRIES,
    max_archive_bytes: info.max_archive_bytes,
    max_archive_entries: info.max_archive_entries,
    upload_rate_bytes_per_sec: info.upload_rate_bytes_per_sec,
    download_rate_bytes_per_sec: info.download_rate_bytes_per_sec,
  }
}

function displayInputs(limits: UpdateTransferLimitsRequest): TransferLimitsInputs {
  return {
    uploadGiB: formatGiB(limits.max_upload_bytes),
    uploadBatchGiB: formatGiB(limits.max_upload_batch_bytes),
    uploadBatchEntries: String(limits.max_upload_batch_entries),
    archiveGiB: formatGiB(limits.max_archive_bytes),
    archiveEntries: String(limits.max_archive_entries),
    uploadRate: formatRate(limits.upload_rate_bytes_per_sec),
    downloadRate: formatRate(limits.download_rate_bytes_per_sec),
  }
}

export function useTransferLimitsForm(getInfo: () => TransferLimitsInfo) {
  const locale = useLocale()
  let baseline = currentLimits(getInfo())
  const initial = reactive(displayInputs(baseline))
  const draft = reactive({ ...initial })
  const hasChanges = computed(() => (
    String(draft.uploadGiB).trim() !== initial.uploadGiB
    || String(draft.uploadBatchGiB).trim() !== initial.uploadBatchGiB
    || String(draft.uploadBatchEntries).trim() !== initial.uploadBatchEntries
    || String(draft.archiveGiB).trim() !== initial.archiveGiB
    || String(draft.archiveEntries).trim() !== initial.archiveEntries
    || String(draft.uploadRate).trim() !== initial.uploadRate
    || String(draft.downloadRate).trim() !== initial.downloadRate
  ))

  function accept(limits: UpdateTransferLimitsRequest): void {
    baseline = { ...limits }
    Object.assign(initial, displayInputs(limits))
    Object.assign(draft, initial)
  }

  function reset(): void {
    accept(currentLimits(getInfo()))
  }

  function parseBytes(value: string | number, originalText: string | number, originalBytes: number, maximum: number, label: string): number {
    if (String(value).trim() === originalText) return originalBytes
    const gib = Number(value)
    const bytes = Math.round(gib * GIB)
    if (!Number.isFinite(gib) || !Number.isSafeInteger(bytes) || bytes < MIB || bytes > maximum) {
      throw new Error(locale.text(`${label}必须在 1 MiB 到 ${maximum / GIB} GiB 之间`, `${label} must be between 1 MiB and ${maximum / GIB} GiB`))
    }
    return bytes
  }

  function parseRate(value: string | number, originalText: string | number, originalBytes: number, label: string): number {
    // Display precision is not the persisted value. Unedited rates must not be
    // rounded and silently resubmitted when another setting changes.
    if (String(value).trim() === originalText) return originalBytes
    const mib = Number(value)
    const bytes = Math.round(mib * MIB)
    if (!Number.isFinite(mib) || !Number.isSafeInteger(bytes)
      || (bytes !== 0 && (bytes < MIN_RATE_BYTES || bytes > HARD_MAX_RATE_BYTES))) {
      throw new Error(locale.text(`${label}必须为 0，或在 0.0625 到 1024 MiB/s 之间`, `${label} must be 0, or between 0.0625 and 1024 MiB/s`))
    }
    return bytes
  }

  function buildRequest(): UpdateTransferLimitsRequest {
    const info = getInfo()
    const maxUploadBytes = parseBytes(draft.uploadGiB, initial.uploadGiB, baseline.max_upload_bytes,
      info.deployment_max_upload_bytes ?? DEFAULT_MAX_BYTES, locale.text('单文件上传上限', 'Single-file upload limit'))
    const maxUploadBatchBytes = parseBytes(draft.uploadBatchGiB, initial.uploadBatchGiB, baseline.max_upload_batch_bytes,
      info.deployment_max_upload_batch_bytes ?? DEFAULT_MAX_BYTES, locale.text('批量上传总大小上限', 'Batch upload size limit'))
    if (maxUploadBatchBytes < maxUploadBytes) {
      throw new Error(locale.text('批量上传总大小上限不能小于单文件上传上限', 'Batch upload size limit cannot be lower than the single-file limit'))
    }
    const batchEntries = Number(draft.uploadBatchEntries)
    const maxBatchEntries = info.deployment_max_upload_batch_entries ?? DEFAULT_MAX_BATCH_ENTRIES
    if (!Number.isInteger(batchEntries) || batchEntries < 1 || batchEntries > maxBatchEntries) {
      throw new Error(locale.text(`批量上传文件数量必须在 1 到 ${maxBatchEntries} 之间`, `Batch upload file count must be between 1 and ${maxBatchEntries}`))
    }
    const maxArchiveBytes = parseBytes(draft.archiveGiB, initial.archiveGiB, baseline.max_archive_bytes,
      info.deployment_max_archive_bytes ?? DEFAULT_MAX_BYTES, locale.text('打包源文件总大小上限', 'Archive source-size limit'))
    const entries = Number(draft.archiveEntries)
    const maxArchiveEntries = info.deployment_max_archive_entries ?? DEFAULT_MAX_ARCHIVE_ENTRIES
    if (!Number.isInteger(entries) || entries < 1 || entries > maxArchiveEntries) {
      throw new Error(locale.text(`打包条目数量上限必须在 1 到 ${maxArchiveEntries} 之间`, `Archive entry limit must be between 1 and ${maxArchiveEntries}`))
    }
    return {
      max_upload_bytes: maxUploadBytes,
      max_upload_batch_bytes: maxUploadBatchBytes,
      max_upload_batch_entries: batchEntries,
      max_archive_bytes: maxArchiveBytes,
      max_archive_entries: entries,
      upload_rate_bytes_per_sec: parseRate(draft.uploadRate, initial.uploadRate, baseline.upload_rate_bytes_per_sec, locale.text('上传限速', 'Upload rate limit')),
      download_rate_bytes_per_sec: parseRate(draft.downloadRate, initial.downloadRate, baseline.download_rate_bytes_per_sec, locale.text('下载限速', 'Download rate limit')),
    }
  }

  function changes(limits: UpdateTransferLimitsRequest): Partial<UpdateTransferLimitsRequest> {
    const info = getInfo()
    const body: Partial<UpdateTransferLimitsRequest> = {}
    for (const field of LIMIT_FIELDS) {
      if (limits[field] !== info[field]) body[field] = limits[field]
    }
    return body
  }

  return { ...toRefs(draft), initial, hasChanges, reset, accept, buildRequest, changes }
}

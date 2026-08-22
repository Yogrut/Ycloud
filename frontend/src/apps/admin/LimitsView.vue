<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { AdminInfo, UpdateTransferLimitsRequest } from '../../shared/api/admin'
import { updateTransferLimits } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const GIB = 1024 ** 3
const MIB = 1024 ** 2
const HARD_MAX_UPLOAD_BYTES = 100 * GIB
const HARD_MAX_ARCHIVE_BYTES = 10 * GIB
const HARD_MAX_ARCHIVE_ENTRIES = 5000
const HARD_MAX_RATE_BYTES = 1024 * MIB
const MIN_RATE_BYTES = 64 * 1024

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()

const uploadGiB = ref<string | number>('')
const archiveGiB = ref<string | number>('')
const archiveEntries = ref<string | number>('')
const uploadRate = ref<string | number>('')
const downloadRate = ref<string | number>('')
const initialUploadGiB = ref('')
const initialArchiveGiB = ref('')
const initialArchiveEntries = ref('')
const initialUploadRate = ref('')
const initialDownloadRate = ref('')
const baselineUploadBytes = ref(0)
const baselineArchiveBytes = ref(0)
const saving = ref(false)
const errorMessage = ref('')

function formatGiB(bytes: number): string {
  return Number((bytes / GIB).toFixed(3)).toString()
}

function formatRate(bytes: number): string {
  return bytes === 0 ? '0' : Number((bytes / MIB).toFixed(4)).toString()
}

function reset(): void {
  baselineUploadBytes.value = props.info.max_upload_bytes
  baselineArchiveBytes.value = props.info.max_archive_bytes
  initialUploadGiB.value = formatGiB(props.info.max_upload_bytes)
  initialArchiveGiB.value = formatGiB(props.info.max_archive_bytes)
  initialArchiveEntries.value = String(props.info.max_archive_entries)
  initialUploadRate.value = formatRate(props.info.upload_rate_bytes_per_sec)
  initialDownloadRate.value = formatRate(props.info.download_rate_bytes_per_sec)
  uploadGiB.value = initialUploadGiB.value
  archiveGiB.value = initialArchiveGiB.value
  archiveEntries.value = initialArchiveEntries.value
  uploadRate.value = initialUploadRate.value
  downloadRate.value = initialDownloadRate.value
  errorMessage.value = ''
}

watch(() => props.info, reset, { immediate: true })

const hasChanges = computed(() => (
  String(uploadGiB.value).trim() !== initialUploadGiB.value
  || String(archiveGiB.value).trim() !== initialArchiveGiB.value
  || String(archiveEntries.value).trim() !== initialArchiveEntries.value
  || String(uploadRate.value).trim() !== initialUploadRate.value
  || String(downloadRate.value).trim() !== initialDownloadRate.value
))

function parseBytes(value: string | number, originalText: string, originalBytes: number, maximum: number, label: string): number {
  if (String(value).trim() === originalText) return originalBytes
  const gib = Number(value)
  const bytes = Math.round(gib * GIB)
  if (!Number.isFinite(gib) || !Number.isSafeInteger(bytes) || bytes < MIB || bytes > maximum) {
    throw new Error(locale.text(`${label}必须在 1 MiB 到 ${maximum / GIB} GiB 之间`, `${label} must be between 1 MiB and ${maximum / GIB} GiB`))
  }
  return bytes
}

function buildRequest(): UpdateTransferLimitsRequest {
  const maxUploadBytes = parseBytes(
    uploadGiB.value,
    initialUploadGiB.value,
    baselineUploadBytes.value,
    HARD_MAX_UPLOAD_BYTES,
    locale.text('单文件上传上限', 'Single-file upload limit'),
  )
  const maxArchiveBytes = parseBytes(
    archiveGiB.value,
    initialArchiveGiB.value,
    baselineArchiveBytes.value,
    HARD_MAX_ARCHIVE_BYTES,
    locale.text('打包源文件总大小上限', 'Archive source-size limit'),
  )
  const entries = Number(archiveEntries.value)
  if (!Number.isInteger(entries) || entries < 1 || entries > HARD_MAX_ARCHIVE_ENTRIES) {
    throw new Error(locale.text('打包条目数量上限必须在 1 到 5000 之间', 'Archive entry limit must be between 1 and 5000'))
  }
  const parseRate = (value: string | number, label: string): number => {
    const mib = Number(value)
    const bytes = Math.round(mib * MIB)
    if (!Number.isFinite(mib) || !Number.isSafeInteger(bytes)
      || (bytes !== 0 && (bytes < MIN_RATE_BYTES || bytes > HARD_MAX_RATE_BYTES))) {
      throw new Error(locale.text(`${label}必须为 0，或在 0.0625 到 1024 MiB/s 之间`, `${label} must be 0, or between 0.0625 and 1024 MiB/s`))
    }
    return bytes
  }
  return {
    max_upload_bytes: maxUploadBytes,
    max_archive_bytes: maxArchiveBytes,
    max_archive_entries: entries,
    upload_rate_bytes_per_sec: parseRate(uploadRate.value, locale.text('上传限速', 'Upload rate limit')),
    download_rate_bytes_per_sec: parseRate(downloadRate.value, locale.text('下载限速', 'Download rate limit')),
  }
}

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  let body: UpdateTransferLimitsRequest
  try {
    body = buildRequest()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('传输限制无效', 'Invalid transfer limits')
    return
  }

  saving.value = true
  errorMessage.value = ''
  try {
    await updateTransferLimits(body)
    baselineUploadBytes.value = body.max_upload_bytes
    baselineArchiveBytes.value = body.max_archive_bytes
    initialUploadGiB.value = formatGiB(body.max_upload_bytes)
    initialArchiveGiB.value = formatGiB(body.max_archive_bytes)
    initialArchiveEntries.value = String(body.max_archive_entries)
    initialUploadRate.value = formatRate(body.upload_rate_bytes_per_sec)
    initialDownloadRate.value = formatRate(body.download_rate_bytes_per_sec)
    uploadGiB.value = initialUploadGiB.value
    archiveGiB.value = initialArchiveGiB.value
    archiveEntries.value = initialArchiveEntries.value
    uploadRate.value = initialUploadRate.value
    downloadRate.value = initialDownloadRate.value
    emit('saved', locale.text('传输限制已保存并立即生效', 'Transfer limits saved and applied'))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <section class="admin-pane form-pane limits-pane glass" aria-labelledby="limits-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="limits-title">{{ locale.text('传输限制', 'Transfer limits') }}</h1>
        <p>{{ locale.text('根据存储容量调整单次传输范围；并发、密码队列和磁盘安全余量继续由系统固定保护。', 'Adjust transfer sizes for your storage capacity. Concurrency, password queues, and disk reserves remain protected by fixed system limits.') }}</p>
      </div>
    </header>
    <form class="admin-pane-body" @submit.prevent="submit">
      <div class="settings-grid limits-grid">
        <label class="compact-field limits-field">
          <span>{{ locale.text('单文件上传上限', 'Single-file upload limit') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadGiB" type="number" min="0.001" max="100" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text('网页与 WebDAV 共用；范围 1 MiB–100 GiB，且不能超过部署环境的绝对上限。', 'Shared by the browser and WebDAV. Range: 1 MiB–100 GiB, subject to the deployment hard limit.') }}</small>
          </span>
        </label>
        <label class="compact-field limits-field">
          <span>{{ locale.text('全局上传限速', 'Global upload rate') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadRate" type="number" min="0" max="1024" step="0.0625" inputmode="decimal"><span>MiB/s</span></span>
            <small>{{ locale.text('网页上传与 WebDAV PUT 共用；0 表示不限速。', 'Shared by browser uploads and WebDAV PUT. Set to 0 for unlimited.') }}</small>
          </span>
        </label>
        <label class="compact-field limits-field">
          <span>{{ locale.text('全局下载限速', 'Global download rate') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="downloadRate" type="number" min="0" max="1024" step="0.0625" inputmode="decimal"><span>MiB/s</span></span>
            <small>{{ locale.text('普通下载、预览、打包下载与 WebDAV GET 共用；0 表示不限速。', 'Shared by downloads, previews, archives, and WebDAV GET. Set to 0 for unlimited.') }}</small>
          </span>
        </label>
        <label class="compact-field limits-field">
          <span>{{ locale.text('打包源文件总大小', 'Archive source-size limit') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="archiveGiB" type="number" min="0.001" max="10" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text('只累计文件内容；范围 1 MiB–10 GiB，普通单文件下载不受影响。', 'Counts file contents only. Range: 1 MiB–10 GiB. Regular single-file downloads are unaffected.') }}</small>
          </span>
        </label>
        <label class="compact-field limits-field">
          <span>{{ locale.text('打包条目数量', 'Archive entry limit') }}</span>
          <span class="limits-control">
            <input v-model="archiveEntries" type="number" min="1" max="5000" step="1" inputmode="numeric">
            <small>{{ locale.text('文件与文件夹递归合计；重复选择父目录与子项时会自动去重。', 'Counts files and folders recursively. Overlapping parent and child selections are deduplicated.') }}</small>
          </span>
        </label>
      </div>
      <aside class="safety-note">
        {{ locale.text('资源保护保持固定：同时只运行一个打包任务，文件提交保持事务化，密码验证与 WebDAV 请求维持严格并发边界，磁盘始终保留安全余量。', 'Resource safeguards remain fixed: one archive task at a time, transactional file commits, strict password and WebDAV concurrency limits, and a reserved disk safety margin.') }}
      </aside>
      <p class="admin-form-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="admin-save-row">
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? locale.t('common.saving') : locale.text('保存传输限制', 'Save transfer limits') }}</button>
      </div>
    </form>
  </section>
</template>

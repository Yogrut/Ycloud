<script setup lang="ts">
import TrafficPanel from './TrafficPanel.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, ref, watch } from 'vue'
import SettingRow from '../../shared/components/SettingRow.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import type { AdminInfo, UpdateTransferLimitsRequest } from '../../shared/api/admin'
import { updateTransferLimits } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const GIB = 1024 ** 3
const MIB = 1024 ** 2
const DEFAULT_BATCH_BYTES = 20 * GIB
const DEFAULT_BATCH_ENTRIES = 1000
const HARD_MAX_RATE_BYTES = 1024 * MIB
const MIN_RATE_BYTES = 64 * 1024

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const trafficPanel = ref<InstanceType<typeof TrafficPanel>>()

const uploadGiB = ref<string | number>('')
const uploadBatchGiB = ref<string | number>('')
const uploadBatchEntries = ref<string | number>('')
const archiveGiB = ref<string | number>('')
const archiveEntries = ref<string | number>('')
const uploadRate = ref<string | number>('')
const downloadRate = ref<string | number>('')
const initialUploadGiB = ref('')
const initialUploadBatchGiB = ref('')
const initialUploadBatchEntries = ref('')
const initialArchiveGiB = ref('')
const initialArchiveEntries = ref('')
const initialUploadRate = ref('')
const initialDownloadRate = ref('')
const baselineUploadBytes = ref(0)
const baselineUploadBatchBytes = ref(0)
const baselineArchiveBytes = ref(0)
const saving = ref(false)
const errorMessage = ref('')
const editor = ref<number>()
const fields = computed(() => [
  { label: locale.text('单文件上传上限', 'Single-file upload limit'), value: `${initialUploadGiB.value} GiB` },
  { label: locale.text('批量上传总大小', 'Batch upload size'), value: `${initialUploadBatchGiB.value} GiB` },
  { label: locale.text('批量上传文件数量', 'Batch upload file count'), value: initialUploadBatchEntries.value },
  { label: locale.text('全局上传限速', 'Global upload rate'), value: `${initialUploadRate.value} MiB/s` },
  { label: locale.text('全局下载限速', 'Global download rate'), value: `${initialDownloadRate.value} MiB/s` },
  { label: locale.text('打包源文件总大小', 'Archive source-size limit'), value: `${initialArchiveGiB.value} GiB` },
  { label: locale.text('打包条目数量', 'Archive entry limit'), value: initialArchiveEntries.value },
])
function openEditor(index: number): void { reset(); editor.value = index }

function formatGiB(bytes: number): string {
  return Number((bytes / GIB).toFixed(3)).toString()
}

function formatRate(bytes: number): string {
  return bytes === 0 ? '0' : Number((bytes / MIB).toFixed(4)).toString()
}

function reset(): void {
  baselineUploadBytes.value = props.info.max_upload_bytes
  baselineUploadBatchBytes.value = props.info.max_upload_batch_bytes ?? DEFAULT_BATCH_BYTES
  baselineArchiveBytes.value = props.info.max_archive_bytes
  initialUploadGiB.value = formatGiB(props.info.max_upload_bytes)
  initialUploadBatchGiB.value = formatGiB(baselineUploadBatchBytes.value)
  initialUploadBatchEntries.value = String(props.info.max_upload_batch_entries ?? DEFAULT_BATCH_ENTRIES)
  initialArchiveGiB.value = formatGiB(props.info.max_archive_bytes)
  initialArchiveEntries.value = String(props.info.max_archive_entries)
  initialUploadRate.value = formatRate(props.info.upload_rate_bytes_per_sec)
  initialDownloadRate.value = formatRate(props.info.download_rate_bytes_per_sec)
  uploadGiB.value = initialUploadGiB.value
  uploadBatchGiB.value = initialUploadBatchGiB.value
  uploadBatchEntries.value = initialUploadBatchEntries.value
  archiveGiB.value = initialArchiveGiB.value
  archiveEntries.value = initialArchiveEntries.value
  uploadRate.value = initialUploadRate.value
  downloadRate.value = initialDownloadRate.value
  errorMessage.value = ''
}

watch(() => props.info, reset, { immediate: true })

const hasChanges = computed(() => (
  String(uploadGiB.value).trim() !== initialUploadGiB.value
  || String(uploadBatchGiB.value).trim() !== initialUploadBatchGiB.value
  || String(uploadBatchEntries.value).trim() !== initialUploadBatchEntries.value
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
    props.info.deployment_max_upload_bytes ?? 100 * GIB,
    locale.text('单文件上传上限', 'Single-file upload limit'),
  )
  const maxUploadBatchBytes = parseBytes(
    uploadBatchGiB.value,
    initialUploadBatchGiB.value,
    baselineUploadBatchBytes.value,
    props.info.deployment_max_upload_batch_bytes ?? 100 * GIB,
    locale.text('批量上传总大小上限', 'Batch upload size limit'),
  )
  if (maxUploadBatchBytes < maxUploadBytes) {
    throw new Error(locale.text('批量上传总大小上限不能小于单文件上传上限', 'Batch upload size limit cannot be lower than the single-file limit'))
  }
  const batchEntries = Number(uploadBatchEntries.value)
  const maxBatchEntries = props.info.deployment_max_upload_batch_entries ?? 10_000
  if (!Number.isInteger(batchEntries) || batchEntries < 1 || batchEntries > maxBatchEntries) {
    throw new Error(locale.text(`批量上传文件数量必须在 1 到 ${maxBatchEntries} 之间`, `Batch upload file count must be between 1 and ${maxBatchEntries}`))
  }
  const maxArchiveBytes = parseBytes(
    archiveGiB.value,
    initialArchiveGiB.value,
    baselineArchiveBytes.value,
    props.info.deployment_max_archive_bytes ?? 100 * GIB,
    locale.text('打包源文件总大小上限', 'Archive source-size limit'),
  )
  const entries = Number(archiveEntries.value)
  const maxArchiveEntries = props.info.deployment_max_archive_entries ?? 100_000
  if (!Number.isInteger(entries) || entries < 1 || entries > maxArchiveEntries) {
    throw new Error(locale.text(`打包条目数量上限必须在 1 到 ${maxArchiveEntries} 之间`, `Archive entry limit must be between 1 and ${maxArchiveEntries}`))
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
    max_upload_batch_bytes: maxUploadBatchBytes,
    max_upload_batch_entries: batchEntries,
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
    baselineUploadBatchBytes.value = body.max_upload_batch_bytes
    baselineArchiveBytes.value = body.max_archive_bytes
    initialUploadGiB.value = formatGiB(body.max_upload_bytes)
    initialUploadBatchGiB.value = formatGiB(body.max_upload_batch_bytes)
    initialUploadBatchEntries.value = String(body.max_upload_batch_entries)
    initialArchiveGiB.value = formatGiB(body.max_archive_bytes)
    initialArchiveEntries.value = String(body.max_archive_entries)
    initialUploadRate.value = formatRate(body.upload_rate_bytes_per_sec)
    initialDownloadRate.value = formatRate(body.download_rate_bytes_per_sec)
    uploadGiB.value = initialUploadGiB.value
    uploadBatchGiB.value = initialUploadBatchGiB.value
    uploadBatchEntries.value = initialUploadBatchEntries.value
    archiveGiB.value = initialArchiveGiB.value
    archiveEntries.value = initialArchiveEntries.value
    uploadRate.value = initialUploadRate.value
    downloadRate.value = initialDownloadRate.value
    emit('saved', locale.text('传输限制已保存并立即生效', 'Transfer limits saved and applied'))
    editor.value = undefined
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
    <div class="settings-rows">
      <SettingRow :label="locale.text('流量限制', 'Traffic limits')" :value="locale.text('统一设置', 'Unified settings')" @edit="trafficPanel?.openEditor()" />
      <SettingRow v-for="(field, index) in fields" :key="index" :label="field.label" :value="field.value" @edit="openEditor(index)" />
    </div>
    <TrafficPanel ref="trafficPanel" mode="settings" @saved="emit('saved', $event)" />
  </section>
  <SettingsDrawer v-if="editor !== undefined" :title="fields[editor]!.label" :busy="saving" @close="editor = undefined">
    <form class="drawer-form" @submit.prevent="feedbackRevision++; submit()">
      <div class="settings-grid limits-grid">
        <label v-if="editor === 0" class="compact-field limits-field limit-upload-size">
          <span>{{ locale.text('单文件上传上限', 'Single-file upload limit') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_upload_bytes ?? 100 * GIB) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text(`网页与 WebDAV 共用；部署上限 ${formatGiB(info.deployment_max_upload_bytes ?? 100 * GIB)} GiB。`, `Shared by browser and WebDAV. Deployment maximum: ${formatGiB(info.deployment_max_upload_bytes ?? 100 * GIB)} GiB.`) }}</small>
          </span>
        </label>
        <label v-if="editor === 1" class="compact-field limits-field limit-upload-batch-size">
          <span>{{ locale.text('批量上传总大小', 'Batch upload size') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadBatchGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_upload_batch_bytes ?? 100 * GIB) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text('一次选择文件或文件夹的内容总量；必须不小于单文件上限。', 'Total content in one file or folder selection; must not be lower than the single-file limit.') }}</small>
          </span>
        </label>
        <label v-if="editor === 2" class="compact-field limits-field limit-upload-batch-entries">
          <span>{{ locale.text('批量上传文件数量', 'Batch upload file count') }}</span>
          <span class="limits-control">
            <input v-model="uploadBatchEntries" aria-required="true" type="number" min="1" :max="info.deployment_max_upload_batch_entries ?? 10000" step="1" inputmode="numeric">
            <small>{{ locale.text('文件夹上传按其中的文件计数，空目录不计入。', 'Folder uploads count contained files; empty directories are not counted.') }}</small>
          </span>
        </label>
        <label v-if="editor === 3" class="compact-field limits-field limit-upload-rate">
          <span>{{ locale.text('全局上传限速', 'Global upload rate') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadRate" type="number" min="0" max="1024" step="0.0625" inputmode="decimal"><span>MiB/s</span></span>
            <small>{{ locale.text('网页上传与 WebDAV PUT 共用；0 表示不限速。', 'Shared by browser uploads and WebDAV PUT. Set to 0 for unlimited.') }}</small>
          </span>
        </label>
        <label v-if="editor === 4" class="compact-field limits-field limit-download-rate">
          <span>{{ locale.text('全局下载限速', 'Global download rate') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="downloadRate" type="number" min="0" max="1024" step="0.0625" inputmode="decimal"><span>MiB/s</span></span>
            <small>{{ locale.text('普通下载、预览、打包下载与 WebDAV GET 共用；0 表示不限速。', 'Shared by downloads, previews, archives, and WebDAV GET. Set to 0 for unlimited.') }}</small>
          </span>
        </label>
        <label v-if="editor === 5" class="compact-field limits-field limit-archive-size">
          <span>{{ locale.text('打包源文件总大小', 'Archive source-size limit') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="archiveGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_archive_bytes ?? 100 * GIB) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text(`只累计文件内容；部署上限 ${formatGiB(info.deployment_max_archive_bytes ?? 100 * GIB)} GiB，普通单文件下载不受影响。`, `Counts file contents only. Deployment maximum: ${formatGiB(info.deployment_max_archive_bytes ?? 100 * GIB)} GiB. Regular downloads are unaffected.`) }}</small>
          </span>
        </label>
        <label v-if="editor === 6" class="compact-field limits-field limit-archive-entries">
          <span>{{ locale.text('打包条目数量', 'Archive entry limit') }}</span>
          <span class="limits-control">
            <input v-model="archiveEntries" aria-required="true" type="number" min="1" :max="info.deployment_max_archive_entries ?? 100000" step="1" inputmode="numeric">
            <small>{{ locale.text('文件与文件夹递归合计；重复选择父目录与子项时会自动去重。', 'Counts files and folders recursively. Overlapping parent and child selections are deduplicated.') }}</small>
          </span>
        </label>
      </div>
      <AppFeedback :revision="feedbackRevision" :message="errorMessage" />
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="editor = undefined">{{ locale.t('common.cancel') }}</button>
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ locale.text('确认', 'Confirm') }}</button>
      </div>
    </form>
  </SettingsDrawer>
</template>

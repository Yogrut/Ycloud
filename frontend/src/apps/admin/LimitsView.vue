<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { AdminInfo, UpdateTransferLimitsRequest } from '../../shared/api/admin'
import { updateTransferLimits } from '../../shared/api/admin'

const GIB = 1024 ** 3
const MIB = 1024 ** 2
const HARD_MAX_UPLOAD_BYTES = 100 * GIB
const HARD_MAX_ARCHIVE_BYTES = 10 * GIB
const HARD_MAX_ARCHIVE_ENTRIES = 5000

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()

const uploadGiB = ref<string | number>('')
const archiveGiB = ref<string | number>('')
const archiveEntries = ref<string | number>('')
const initialUploadGiB = ref('')
const initialArchiveGiB = ref('')
const initialArchiveEntries = ref('')
const baselineUploadBytes = ref(0)
const baselineArchiveBytes = ref(0)
const saving = ref(false)
const errorMessage = ref('')

function formatGiB(bytes: number): string {
  return Number((bytes / GIB).toFixed(3)).toString()
}

function reset(): void {
  baselineUploadBytes.value = props.info.max_upload_bytes
  baselineArchiveBytes.value = props.info.max_archive_bytes
  initialUploadGiB.value = formatGiB(props.info.max_upload_bytes)
  initialArchiveGiB.value = formatGiB(props.info.max_archive_bytes)
  initialArchiveEntries.value = String(props.info.max_archive_entries)
  uploadGiB.value = initialUploadGiB.value
  archiveGiB.value = initialArchiveGiB.value
  archiveEntries.value = initialArchiveEntries.value
  errorMessage.value = ''
}

watch(() => props.info, reset, { immediate: true })

const hasChanges = computed(() => (
  String(uploadGiB.value).trim() !== initialUploadGiB.value
  || String(archiveGiB.value).trim() !== initialArchiveGiB.value
  || String(archiveEntries.value).trim() !== initialArchiveEntries.value
))

function parseBytes(value: string | number, originalText: string, originalBytes: number, maximum: number, label: string): number {
  if (String(value).trim() === originalText) return originalBytes
  const gib = Number(value)
  const bytes = Math.round(gib * GIB)
  if (!Number.isFinite(gib) || !Number.isSafeInteger(bytes) || bytes < MIB || bytes > maximum) {
    throw new Error(`${label}必须在 1 MiB 到 ${maximum / GIB} GiB 之间`)
  }
  return bytes
}

function buildRequest(): UpdateTransferLimitsRequest {
  const maxUploadBytes = parseBytes(
    uploadGiB.value,
    initialUploadGiB.value,
    baselineUploadBytes.value,
    HARD_MAX_UPLOAD_BYTES,
    '单文件上传上限',
  )
  const maxArchiveBytes = parseBytes(
    archiveGiB.value,
    initialArchiveGiB.value,
    baselineArchiveBytes.value,
    HARD_MAX_ARCHIVE_BYTES,
    '打包源文件总大小上限',
  )
  const entries = Number(archiveEntries.value)
  if (!Number.isInteger(entries) || entries < 1 || entries > HARD_MAX_ARCHIVE_ENTRIES) {
    throw new Error('打包条目数量上限必须在 1 到 5000 之间')
  }
  return {
    max_upload_bytes: maxUploadBytes,
    max_archive_bytes: maxArchiveBytes,
    max_archive_entries: entries,
  }
}

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  let body: UpdateTransferLimitsRequest
  try {
    body = buildRequest()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '传输限制无效'
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
    uploadGiB.value = initialUploadGiB.value
    archiveGiB.value = initialArchiveGiB.value
    archiveEntries.value = initialArchiveEntries.value
    emit('saved', '传输限制已保存并立即生效')
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '保存失败'
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="limits-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="limits-title">传输限制</h1>
        <p>根据存储容量调整单次传输范围；并发、密码队列和磁盘安全余量继续由系统固定保护。</p>
      </div>
    </header>
    <form class="admin-pane-body" @submit.prevent="submit">
      <div class="account-form">
        <label class="admin-field limits-field">
          <span>单文件上传上限</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadGiB" type="number" min="0.001" max="100" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>网页与 WebDAV 共用；范围 1 MiB–100 GiB，且不能超过部署环境的绝对上限。</small>
          </span>
        </label>
        <label class="admin-field limits-field">
          <span>打包源文件总大小</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="archiveGiB" type="number" min="0.001" max="10" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>只累计文件内容；范围 1 MiB–10 GiB，普通单文件下载不受影响。</small>
          </span>
        </label>
        <label class="admin-field limits-field">
          <span>打包条目数量</span>
          <span class="limits-control">
            <input v-model="archiveEntries" type="number" min="1" max="5000" step="1" inputmode="numeric">
            <small>文件与文件夹递归合计；重复选择父目录与子项时会自动去重。</small>
          </span>
        </label>
      </div>
      <aside class="safety-note">
        资源保护保持固定：同时只运行一个打包任务，文件提交保持事务化，密码验证与 WebDAV 请求维持严格并发边界，磁盘始终保留安全余量。
      </aside>
      <p class="admin-form-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="admin-save-row">
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? '保存中…' : '保存传输限制' }}</button>
      </div>
    </form>
  </section>
</template>

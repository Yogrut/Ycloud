<script setup lang="ts">
import TrafficPanel from './TrafficPanel.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, ref, watch } from 'vue'
import SettingRow from '../../shared/components/SettingRow.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import type { AdminInfo, UpdateTransferLimitsRequest } from '../../shared/api/admin'
import { updateTransferLimits } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { DEFAULT_MAX_ARCHIVE_ENTRIES, DEFAULT_MAX_BATCH_ENTRIES, DEFAULT_MAX_BYTES, GIB, formatGiB, useTransferLimitsForm } from './useTransferLimitsForm'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const trafficPanel = ref<InstanceType<typeof TrafficPanel>>()
const form = useTransferLimitsForm(() => props.info)
const {
  uploadGiB, uploadBatchGiB, uploadBatchEntries, archiveGiB, archiveEntries,
  uploadRate, downloadRate, initial, hasChanges, buildRequest,
} = form
const saving = ref(false)
const errorMessage = ref('')
const editor = ref<number>()
const fields = computed(() => [
  { label: locale.text('单文件上传上限', 'Single-file upload limit'), value: `${initial.uploadGiB} GiB` },
  { label: locale.text('批量上传总大小', 'Batch upload size'), value: `${initial.uploadBatchGiB} GiB` },
  { label: locale.text('批量上传文件数量', 'Batch upload file count'), value: initial.uploadBatchEntries },
  { label: locale.text('全局上传限速', 'Global upload rate'), value: `${initial.uploadRate} MiB/s` },
  { label: locale.text('全局下载限速', 'Global download rate'), value: `${initial.downloadRate} MiB/s` },
  { label: locale.text('打包源文件总大小', 'Archive source-size limit'), value: `${initial.archiveGiB} GiB` },
  { label: locale.text('打包条目数量', 'Archive entry limit'), value: initial.archiveEntries },
])

function openEditor(index: number): void {
  reset()
  editor.value = index
}

function reset(): void {
  form.reset()
  errorMessage.value = ''
}

watch(() => props.info, reset)

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
    await updateTransferLimits(form.changes(body))
    form.accept(body)
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
            <span class="input-with-unit"><input v-model="uploadGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_upload_bytes ?? DEFAULT_MAX_BYTES) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text(`网页与 WebDAV 共用；部署上限 ${formatGiB(info.deployment_max_upload_bytes ?? DEFAULT_MAX_BYTES)} GiB。`, `Shared by browser and WebDAV. Deployment maximum: ${formatGiB(info.deployment_max_upload_bytes ?? DEFAULT_MAX_BYTES)} GiB.`) }}</small>
          </span>
        </label>
        <label v-if="editor === 1" class="compact-field limits-field limit-upload-batch-size">
          <span>{{ locale.text('批量上传总大小', 'Batch upload size') }}</span>
          <span class="limits-control">
            <span class="input-with-unit"><input v-model="uploadBatchGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_upload_batch_bytes ?? DEFAULT_MAX_BYTES) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text('一次选择文件或文件夹的内容总量；必须不小于单文件上限。', 'Total content in one file or folder selection; must not be lower than the single-file limit.') }}</small>
          </span>
        </label>
        <label v-if="editor === 2" class="compact-field limits-field limit-upload-batch-entries">
          <span>{{ locale.text('批量上传文件数量', 'Batch upload file count') }}</span>
          <span class="limits-control">
            <input v-model="uploadBatchEntries" aria-required="true" type="number" min="1" :max="info.deployment_max_upload_batch_entries ?? DEFAULT_MAX_BATCH_ENTRIES" step="1" inputmode="numeric">
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
            <span class="input-with-unit"><input v-model="archiveGiB" aria-required="true" type="number" min="0.001" :max="(info.deployment_max_archive_bytes ?? DEFAULT_MAX_BYTES) / GIB" step="0.001" inputmode="decimal"><span>GiB</span></span>
            <small>{{ locale.text(`只累计文件内容；部署上限 ${formatGiB(info.deployment_max_archive_bytes ?? DEFAULT_MAX_BYTES)} GiB，普通单文件下载不受影响。`, `Counts file contents only. Deployment maximum: ${formatGiB(info.deployment_max_archive_bytes ?? DEFAULT_MAX_BYTES)} GiB. Regular downloads are unaffected.`) }}</small>
          </span>
        </label>
        <label v-if="editor === 6" class="compact-field limits-field limit-archive-entries">
          <span>{{ locale.text('打包条目数量', 'Archive entry limit') }}</span>
          <span class="limits-control">
            <input v-model="archiveEntries" aria-required="true" type="number" min="1" :max="info.deployment_max_archive_entries ?? DEFAULT_MAX_ARCHIVE_ENTRIES" step="1" inputmode="numeric">
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

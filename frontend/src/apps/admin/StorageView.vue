<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import AppSwitch from '../../shared/components/AppSwitch.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import { computed, ref, watch } from 'vue'
import type { LocalMountView, StorageInstanceView } from '../../shared/api/admin'
import { activatePendingStorage, addLocalStorage, deleteStorage, discardPendingStorage, stageS3Storage, testLocalStorage, testS3Storage, updateLocalStorage, updateS3Storage } from '../../shared/api/admin'
import AppIcon from '../../shared/components/AppIcon.vue'
import AppSelect from '../../shared/components/AppSelect.vue'
import { useLocale } from '../../shared/i18n'
import { MAX_STORAGE_CAPACITY_GIB, useStorageForm, type StorageOption } from './useStorageForm'

const props = defineProps<{ instances: StorageInstanceView[]; pendingInstance: StorageInstanceView | null; localMounts: LocalMountView[] }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)

const providers: Array<{ id: StorageOption; zh: string; en: string; protocol: string }> = [
  { id: 'local', zh: '本地存储', en: 'Local storage', protocol: 'Filesystem' },
  { id: 'alibaba_oss', zh: '阿里云 OSS', en: 'Alibaba Cloud OSS', protocol: 'S3 SigV4' },
  { id: 'tencent_cos', zh: '腾讯云 COS', en: 'Tencent Cloud COS', protocol: 'S3 SigV4' },
  { id: 'minio', zh: 'MinIO / RustFS', en: 'MinIO / RustFS', protocol: 'S3 SigV4' },
  { id: 's3_compatible', zh: 'S3 通用协议', en: 'Generic S3-compatible', protocol: 'S3 SigV4' },
]

const editorOpen = ref(false)
const editingId = ref('')
const editingRevision = ref<string>()
const {
  provider, storageName, localPath, endpoint, bucket, region, prefix, addressingStyle,
  relayUpload, accessKeyId, secretAccessKey, capacityLimitGiB, enabled, allowGuestAccess,
  allowGuestDownload, isS3, isOfficialCloud, reset: resetForm, loadInstance,
  selectProvider: selectFormProvider, capacityLimitBytes, s3RequestBody: requestBody,
} = useStorageForm()
const busy = ref(false)
const errorMessage = ref('')
const connectionNotice = ref<{ message: string; kind: 'success' | 'error' }>()
let connectionRevision = 0
const pendingDelete = ref<StorageInstanceView>()

watch([editorOpen, provider, localPath, endpoint, bucket, region, prefix, addressingStyle, accessKeyId, secretAccessKey, capacityLimitGiB], () => {
  connectionRevision += 1
  connectionNotice.value = undefined
}, { flush: 'sync' })

const editingInstance = computed(() => props.instances.find(instance => instance.id === editingId.value))
const localMountOptions = computed(() => props.localMounts
  .filter(mount => !mount.storage_id || mount.storage_id === editingId.value)
  .map(mount => ({
    value: mount.path,
    label: `${mount.name} · ${mount.path}${mount.ready ? '' : locale.text('（不可用）', ' (unavailable)')}`,
    disabled: !mount.ready,
  })))
const addressingOptions = [
  { value: 'path', label: 'Path Style' },
  { value: 'virtual_hosted', label: 'Virtual Hosted' },
]

function formatBytes(value: number): string {
  if (value >= 1024 ** 4) return `${(value / (1024 ** 4)).toFixed(2)} TiB`
  if (value >= 1024 ** 3) return `${(value / (1024 ** 3)).toFixed(2)} GiB`
  if (value >= 1024 ** 2) return `${(value / (1024 ** 2)).toFixed(2)} MiB`
  if (value >= 1024) return `${(value / 1024).toFixed(2)} KiB`
  return `${value} B`
}
function statusLabel(instance: StorageInstanceView): string {
  if (instance.status === 'pending') return locale.text('处理中', 'Settling')
  if (instance.status === 'disabled') return locale.text('停用', 'Disabled')
  if (instance.status === 'abnormal') return locale.text('异常', 'Abnormal')
  return locale.text('启用', 'Enabled')
}
function cleanupDebtLabel(instance: StorageInstanceView): string {
  const bytes = formatBytes(instance.cleanup_pending_bytes ?? 0)
  if (instance.cleanup_debt_complete === false) {
    return locale.text(`回收积压仍在核对（已登记上限 ${bytes}）`, `Cleanup backlog is still being checked (${bytes} recorded upper bound)`)
  }
  return locale.text(`待回收（上限）${bytes}`, `Pending cleanup (upper bound): ${bytes}`)
}
function stagingCleanupLabel(instance: StorageInstanceView): string {
  const uploads = instance.staging_cleanup_pending_uploads ?? 0
  const copies = instance.staging_cleanup_pending_copies ?? 0
  const attempts = instance.staging_cleanup_failed_attempts ?? 0
  const suffix = attempts > 0
    ? locale.text(`，累计失败重试 ${attempts} 次`, `; ${attempts} failed attempts`)
    : ''
  return locale.text(`待清理暂存：上传 ${uploads}，复制 ${copies}${suffix}`, `Pending staging cleanup: ${uploads} uploads, ${copies} copies${suffix}`)
}
function s3OrphanLabel(instance: StorageInstanceView): string {
  const uploads = instance.s3_orphan_uploads ?? 0
  const backups = instance.s3_orphan_backups ?? 0
  return locale.text(`待处理对象存储暂存：上传 ${uploads}，备份 ${backups}`, `S3 staging objects requiring attention: ${uploads} uploads, ${backups} backups`)
}
function formatDuration(seconds: number): string {
  if (seconds >= 86400) return locale.text(`${Math.floor(seconds / 86400)} 天`, `${Math.floor(seconds / 86400)}d`)
  if (seconds >= 3600) return locale.text(`${Math.floor(seconds / 3600)} 小时`, `${Math.floor(seconds / 3600)}h`)
  if (seconds >= 60) return locale.text(`${Math.floor(seconds / 60)} 分钟`, `${Math.floor(seconds / 60)}m`)
  return locale.text(`${seconds} 秒`, `${seconds}s`)
}
function s3RecoveryLabel(instance: StorageInstanceView): string {
  const pending = instance.s3_recovery_pending_records ?? 0
  const age = instance.s3_recovery_oldest_pending_seconds
  const failures = instance.s3_recovery_consecutive_failures ?? 0
  const state = instance.s3_recovery_running
    ? locale.text('对象存储恢复中', 'S3 recovery in progress')
    : locale.text(`对象存储待恢复：${pending} 条责任记录`, `S3 recovery pending: ${pending} owned records`)
  const ageText = age == null
    ? ''
    : locale.text(`，最早已等待 ${formatDuration(age)}`, `; oldest waiting ${formatDuration(age)}`)
  const retryText = failures > 0
    ? locale.text(`，连续失败 ${failures} 次`, `; ${failures} consecutive failures`)
    : ''
  const errorText = failures > 0 && instance.s3_recovery_last_failure
    ? locale.text(`：${instance.s3_recovery_last_failure}`, `: ${instance.s3_recovery_last_failure}`)
    : ''
  return `${state}${ageText}${retryText}${errorText}`
}
function resetEditor(): void {
  connectionNotice.value = undefined
  editingId.value = ''
  editingRevision.value = undefined
  resetForm()
  errorMessage.value = ''
}
function openNew(): void {
  resetEditor()
  localPath.value = String(localMountOptions.value[0]?.value ?? '')
  editorOpen.value = true
}
function selectProvider(value: StorageOption): void {
  selectFormProvider(value)
  if (value === 'local' && !localPath.value) localPath.value = String(localMountOptions.value[0]?.value ?? '')
  errorMessage.value = ''
}
function openSettings(instance: StorageInstanceView): void {
  resetEditor()
  editingId.value = instance.id
  editingRevision.value = instance.revision
  loadInstance(instance)
  editorOpen.value = true
}
async function run(action: () => Promise<void>, fallback: string): Promise<void> {
  if (busy.value) return
  feedbackRevision.value += 1
  busy.value = true
  errorMessage.value = ''
  connectionNotice.value = undefined
  try {
    await action()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : fallback
  } finally {
    busy.value = false
  }
}
async function testConnection(): Promise<void> {
  if (busy.value) return
  feedbackRevision.value += 1
  busy.value = true
  errorMessage.value = ''
  connectionNotice.value = undefined
  const revision = connectionRevision
  try {
    const result = provider.value === 'local' ? await testLocalStorage(localPath.value.trim()) : await testS3Storage(requestBody())
    if (!result.success) throw new Error(locale.text('存储连接测试失败', 'Storage connection test failed'))
    if (revision === connectionRevision) connectionNotice.value = { kind: 'success', message: locale.text('测试成功：存储连接与读写能力验证通过', 'Test successful: storage connectivity and read/write capabilities verified') }
  } catch (error) {
    const message = error instanceof Error ? error.message : locale.text('无法验证存储连接', 'Unable to verify storage connectivity')
    if (revision === connectionRevision) connectionNotice.value = { kind: 'error', message: `${locale.text('测试失败', 'Test failed')}：${message}` }
  } finally { busy.value = false }
}
async function saveStorage(): Promise<void> {
  await run(async () => {
    if (editingInstance.value) {
      if (editingInstance.value.backend.type === 'local') await updateLocalStorage(editingInstance.value.id, storageName.value.trim(), localPath.value.trim(), capacityLimitBytes(), enabled.value, allowGuestAccess.value, allowGuestDownload.value, editingRevision.value)
      else await updateS3Storage(editingInstance.value.id, storageName.value.trim(), requestBody(), enabled.value, allowGuestAccess.value, allowGuestDownload.value, editingRevision.value)
      editorOpen.value = false
      emit('changed', locale.text('存储设置已保存', 'Storage settings saved'))
      return
    }
    const name = storageName.value.trim()
    if (!name) throw new Error(locale.text('请输入存储名称', 'Enter a storage name'))
    if (provider.value === 'local') {
      const path = localPath.value.trim()
      if (!path) throw new Error(locale.text('请输入本地存储路径', 'Enter a local storage path'))
      await addLocalStorage(path, name, capacityLimitBytes(), enabled.value, allowGuestAccess.value, allowGuestDownload.value)
    } else {
      await stageS3Storage(name, requestBody(), enabled.value, allowGuestAccess.value, allowGuestDownload.value)
      await activatePendingStorage()
    }
    editorOpen.value = false
    emit('changed', locale.text('存储源已添加；原路径中的文件会直接显示，文件未被移动或删除', 'Storage added. Existing files at the path are shown directly and were not moved or deleted.'))
  }, locale.text('无法保存存储源', 'Unable to save storage'))
}
async function removeInstance(): Promise<void> {
  const instance = pendingDelete.value
  if (!instance) return
  await run(async () => { await deleteStorage(instance.id); pendingDelete.value = undefined; emit('changed', locale.text('存储配置已删除，文件未被删除', 'Storage configuration removed; files were not deleted')) }, locale.text('无法删除存储配置；请先移除相关 WebDAV、文件夹锁或用户权限', 'Unable to remove storage; remove related WebDAV mounts, folder locks, or user permissions first'))
}
async function clearPending(): Promise<void> {
  await run(async () => { await discardPendingStorage(); emit('changed', locale.text('未完成的存储配置已清除', 'Pending storage configuration cleared')) }, locale.text('无法清除未完成配置', 'Unable to clear pending configuration'))
}
</script>

<template>
  <section class="admin-pane list-pane form-pane storage-pane glass" aria-labelledby="storage-title">
    <header class="admin-pane-head storage-page-head">
      <div><h1 id="storage-title">{{ locale.text('存储设置', 'Storage') }}</h1><p>{{ locale.text('每个存储拥有独立命名空间；管理已添加的本地磁盘与对象存储，删除配置不会删除文件。', 'Each storage has an independent namespace. Manage local disks and object storage; removing a configuration never deletes files.') }}</p></div>
      <button class="btn" type="button" @click="openNew">{{ locale.text('新建存储', 'New storage') }}</button>
    </header>
    <div class="admin-pane-body storage-body">
      <div v-if="pendingInstance" class="storage-boundary-note storage-wide">
        <span>{{ locale.text(`检测到未完成的存储配置：${pendingInstance.name}`, `Pending storage configuration: ${pendingInstance.name}`) }}</span>
        <button class="btn secondary compact" type="button" :disabled="busy" @click="clearPending">{{ locale.text('清除', 'Clear') }}</button>
      </div>
      <div v-if="instances.length" class="storage-instance-list storage-wide">
        <div class="record-table-head"><span>{{ locale.text('名称', 'Name') }}</span><span>{{ locale.text('存储用量', 'Storage used') }}</span><span>{{ locale.text('状态 / 操作', 'Status / Actions') }}</span></div>
        <article v-for="instance in instances" :key="instance.id" class="storage-instance-card storage-source-row">
          <strong class="admin-record-name"><span class="storage-health-light" :class="{ healthy: instance.health_ok ?? instance.ready }" :title="`${(instance.health_ok ?? instance.ready) ? locale.text('连接正常', 'Connection healthy') : locale.text('连接异常', 'Connection error')} · ${instance.health_checked_at ? new Date(instance.health_checked_at * 1000).toLocaleString() : locale.text('尚未检查', 'Not checked')}`" />{{ instance.name }}</strong>
          <div class="admin-record-value">
            <span>{{ locale.text('存储用量', 'Storage used') }}</span>
            <strong v-if="instance.capacity_accurate !== false">{{ formatBytes(instance.usage_bytes) }}</strong>
            <strong v-else-if="instance.capacity_reconciling">{{ locale.text('核对中', 'Checking') }}</strong>
            <strong v-else>{{ locale.text('待核对', 'Needs checking') }}</strong>
            <small v-if="(instance.cleanup_pending_bytes ?? 0) > 0 || instance.cleanup_debt_complete === false" class="storage-cleanup-debt">{{ cleanupDebtLabel(instance) }}</small>
            <small v-if="(instance.staging_cleanup_pending_uploads ?? 0) > 0 || (instance.staging_cleanup_pending_copies ?? 0) > 0" class="storage-cleanup-debt">{{ stagingCleanupLabel(instance) }}</small>
            <small v-if="(instance.s3_orphan_uploads ?? 0) > 0 || (instance.s3_orphan_backups ?? 0) > 0" class="storage-cleanup-debt">{{ s3OrphanLabel(instance) }}</small>
            <small v-if="(instance.s3_recovery_pending_records ?? 0) > 0 || (instance.s3_recovery_consecutive_failures ?? 0) > 0 || instance.s3_recovery_running" class="storage-cleanup-debt">{{ s3RecoveryLabel(instance) }}</small>
          </div>
          <div class="admin-record-end">
            <div class="admin-record-status"><span class="status-pill" :class="{ warning: instance.status === 'abnormal', muted: instance.status === 'disabled' }">{{ statusLabel(instance) }}</span><span v-if="instance.allow_guest_access" class="status-pill">{{ locale.text('访客可访问', 'Guest access') }}</span></div>
            <div class="storage-instance-actions">
              <button class="record-text-btn" type="button" :disabled="busy" :title="locale.text('编辑存储', 'Edit storage')" :aria-label="locale.text('编辑存储', 'Edit storage')" @click="openSettings(instance)">{{ locale.t('common.edit') }}</button>
              <button class="record-text-btn danger" type="button" :disabled="busy" :title="locale.text('删除存储', 'Delete storage')" :aria-label="locale.text('删除存储', 'Delete storage')" @click="pendingDelete = instance; errorMessage = ''">{{ locale.t('common.delete') }}</button>
            </div>
          </div>
        </article>
      </div>
      <div v-else class="storage-empty storage-wide"><AppIcon name="storage" :size="40" /><strong>{{ locale.text('尚未添加存储源', 'No storage sources') }}</strong><span>{{ locale.text('点击右上角“新建存储”开始配置。', 'Use “New storage” to add one.') }}</span></div>
    </div>
  </section>

  <SettingsDrawer v-if="editorOpen" :title="editingInstance ? locale.text('存储设置', 'Storage settings') : locale.text('新建存储', 'New storage')" :busy="busy" wide @close="editorOpen = false">
    <section class="modal storage-editor">
      <div v-if="!editingInstance" class="storage-provider-grid">
        <button v-for="item in providers" :key="item.id" type="button" class="storage-provider" :class="{ active: provider === item.id }" @click="selectProvider(item.id)"><strong>{{ locale.text(item.zh, item.en) }}</strong><small>{{ item.protocol }}</small></button>
      </div>
      <div class="storage-form">
        <label :class="{ 'storage-wide': provider === 'local' }">{{ locale.text('存储名称', 'Storage name') }}<input v-model="storageName" aria-required="true" class="input local-storage-name" maxlength="64"></label>
        <label v-if="provider === 'local'" class="required-field">{{ locale.text('本地存储挂载', 'Local storage mount') }}<AppSelect v-model="localPath" class="local-storage-path" :options="localMountOptions" :label="locale.text('本地存储挂载', 'Local storage mount')" /><small>{{ locale.text('这里只显示启动参数中已声明且尚未使用的挂载点。', 'Only declared and unused deployment mounts are shown.') }}</small></label>
        <label v-if="provider === 'local'">{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input local-capacity-input" type="number" min="0" :max="MAX_STORAGE_CAPACITY_GIB" step="0.001"><small>{{ locale.text('0 表示不设置逻辑上限，最小 1 MiB。', '0 disables the logical limit; minimum 1 MiB.') }}</small></label>
        <template v-if="isS3">
          <label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}<input v-model="endpoint" aria-required="true" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com"><small>{{ locale.text('填写对象存储的 HTTP(S) 接口地址，不包含桶名、账号或密码。', 'Enter the object storage HTTP(S) endpoint without a bucket name or credentials.') }}</small></label><label>Bucket<input v-model="bucket" aria-required="true" class="input" maxlength="63"></label><label>Region<input v-model="region" aria-required="true" class="input" maxlength="64"></label><label class="storage-wide">Prefix<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/"></label><label>Access Key ID<input v-model="accessKeyId" :aria-required="!editingInstance" class="input" maxlength="256" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>Secret Access Key<input v-model="secretAccessKey" :aria-required="!editingInstance" class="input" type="password" maxlength="4096" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>{{ locale.text('寻址方式', 'Addressing style') }}<AppSelect v-model="addressingStyle" :options="addressingOptions" :label="locale.text('寻址方式', 'Addressing style')" :disabled="isOfficialCloud" /></label><label>{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" :max="MAX_STORAGE_CAPACITY_GIB" step="0.001"></label>
        </template>
        <div class="storage-access-options storage-wide">
          <AppSwitch v-model="enabled" :label="locale.text('启动', 'Enabled')" :description="locale.text('停用后不会出现在文件页切换器中。', 'Disabled storage is hidden from the file browser.')" :disabled="busy" />
          <AppSwitch v-if="isS3" v-model="relayUpload" :label="locale.text('中转上传', 'Relay uploads')" :description="locale.text('无法直传时开启，经服务器转发。关闭则直传，需浏览器可达和跨域配置，不受本站限速。', 'Enable server relay when direct uploads are unavailable. Direct uploads require browser access and CORS, and bypass server rate limits.')" :disabled="busy" />
          <AppSwitch v-model="allowGuestAccess" :label="locale.text('允许访客访问', 'Allow guest access')" :description="locale.text('关闭后需要管理员或已授权用户账号。', 'When off, an administrator or authorized user must sign in.')" :disabled="busy" />
          <AppSwitch v-model="allowGuestDownload" :label="locale.text('允许访客下载', 'Allow guest downloads')" :description="locale.text('需先允许访客访问；关闭后访客只能浏览文件列表，不能下载或预览文件内容。', 'Requires guest access. When off, guests can only browse the file list, not download or preview content.')" :disabled="busy || !allowGuestAccess" />
        </div>
        <div v-if="provider === 'local' && localPath.trim()" class="storage-boundary-note storage-wide">{{ locale.text('保存后直接展示该路径中的现有文件，不进行导入、移动或删除。', 'Existing files at this path are shown directly; nothing is imported, moved, or deleted.') }}</div>
      </div>
      <div class="modal-actions"><button v-if="!editingInstance" class="btn secondary" type="button" :disabled="busy" @click="testConnection">{{ locale.text('测试连接', 'Test connection') }}</button><button class="btn secondary" type="button" :disabled="busy" @click="editorOpen = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="button" :disabled="busy" @click="saveStorage">{{ locale.text('确认', 'Confirm') }}</button></div>
    </section>
  </SettingsDrawer>
  <AppFeedback v-if="!pendingDelete" :revision="feedbackRevision" :message="errorMessage" />
  <AppFeedback v-if="connectionNotice" :revision="feedbackRevision" :message="connectionNotice.message" :kind="connectionNotice.kind" />
  <ConfirmDialog v-if="pendingDelete" :title="locale.text('删除存储', 'Delete storage')" :message="locale.text('将删除以下存储配置，是否继续？', 'Delete the following storage configuration?')" :target="pendingDelete.name" :detail="locale.text('仅移除连接配置，存储中的文件不会被删除。', 'Only the connection configuration is removed. Stored files will not be deleted.')" :error="errorMessage" :busy="busy" @close="pendingDelete = undefined; errorMessage = ''" @confirm="removeInstance" />
</template>

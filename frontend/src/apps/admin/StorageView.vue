<script setup lang="ts">
import { computed, ref } from 'vue'
import type { LocalMountView, S3AddressingStyle, S3Provider, StorageInstanceView, TestS3StorageRequest } from '../../shared/api/admin'
import { activatePendingStorage, addLocalStorage, deleteStorage, discardPendingStorage, stageS3Storage, testS3Storage, updateLocalStorage, updateS3Storage, updateStorageAccess } from '../../shared/api/admin'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ instances: StorageInstanceView[]; pendingInstance: StorageInstanceView | null; localMounts: LocalMountView[] }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
type StorageOption = 'local' | S3Provider

const providers: Array<{ id: StorageOption; zh: string; en: string; protocol: string }> = [
  { id: 'local', zh: '本地存储', en: 'Local storage', protocol: 'Filesystem' },
  { id: 'alibaba_oss', zh: '阿里云 OSS', en: 'Alibaba Cloud OSS', protocol: 'S3 SigV4' },
  { id: 'tencent_cos', zh: '腾讯云 COS', en: 'Tencent Cloud COS', protocol: 'S3 SigV4' },
  { id: 'minio', zh: 'MinIO / RustFS', en: 'MinIO / RustFS', protocol: 'S3 SigV4' },
  { id: 's3_compatible', zh: 'S3 通用协议', en: 'Generic S3-compatible', protocol: 'S3 SigV4' },
]

const editorOpen = ref(false)
const editingId = ref('')
const provider = ref<StorageOption>('local')
const storageName = ref('')
const localPath = ref('')
const endpoint = ref('')
const bucket = ref('')
const region = ref('us-east-1')
const prefix = ref('')
const addressingStyle = ref<S3AddressingStyle>('path')
const accessKeyId = ref('')
const secretAccessKey = ref('')
const capacityLimitGiB = ref(0)
const enabled = ref(true)
const allowGuestAccess = ref(false)
const busy = ref(false)
const errorMessage = ref('')

const editingInstance = computed(() => props.instances.find(instance => instance.id === editingId.value))
const isS3 = computed(() => provider.value !== 'local')
const isOfficialCloud = computed(() => provider.value === 'alibaba_oss' || provider.value === 'tencent_cos')

function bytesToGiB(value: number | null): number { return value ? value / (1024 ** 3) : 0 }
function formatBytes(value: number): string {
  if (value >= 1024 ** 4) return `${(value / (1024 ** 4)).toFixed(2)} TiB`
  if (value >= 1024 ** 3) return `${(value / (1024 ** 3)).toFixed(2)} GiB`
  if (value >= 1024 ** 2) return `${(value / (1024 ** 2)).toFixed(2)} MiB`
  return `${value} B`
}
function providerLabel(value: S3Provider): string {
  const item = providers.find(candidate => candidate.id === value)
  return item ? locale.text(item.zh, item.en) : 'S3'
}
function backendLabel(instance: StorageInstanceView): string {
  return instance.backend.type === 'local' ? locale.text('本地存储', 'Local storage') : providerLabel(instance.backend.provider)
}
function statusLabel(instance: StorageInstanceView): string {
  if (instance.status === 'disabled') return locale.text('停用', 'Disabled')
  if (instance.status === 'abnormal') return locale.text('异常', 'Abnormal')
  return locale.text('启用', 'Enabled')
}
function capacityLimitBytes(): number | null {
  const value = Number(capacityLimitGiB.value)
  if (!Number.isInteger(value) || value < 0 || value > 4_194_304) throw new Error(locale.text('容量上限必须是 0 到 4194304 之间的整数 GiB', 'Capacity must be an integer from 0 to 4194304 GiB'))
  return value === 0 ? null : value * (1024 ** 3)
}
function resetEditor(): void {
  editingId.value = ''
  provider.value = 'local'
  storageName.value = ''
  localPath.value = ''
  endpoint.value = ''
  bucket.value = ''
  region.value = 'us-east-1'
  prefix.value = ''
  addressingStyle.value = 'path'
  accessKeyId.value = ''
  secretAccessKey.value = ''
  capacityLimitGiB.value = 0
  enabled.value = true
  allowGuestAccess.value = false
  errorMessage.value = ''
}
function openNew(): void { resetEditor(); editorOpen.value = true }
function selectProvider(value: StorageOption): void {
  provider.value = value
  addressingStyle.value = value === 'alibaba_oss' || value === 'tencent_cos' ? 'virtual_hosted' : 'path'
  errorMessage.value = ''
}
function openSettings(instance: StorageInstanceView): void {
  resetEditor()
  editingId.value = instance.id
  storageName.value = instance.name
  enabled.value = instance.enabled ?? true
  allowGuestAccess.value = instance.allow_guest_access ?? false
  provider.value = instance.backend.type === 'local' ? 'local' : instance.backend.provider
  if (instance.backend.type === 'local') {
    localPath.value = instance.backend.path
    capacityLimitGiB.value = bytesToGiB(instance.backend.capacity_limit_bytes)
  } else {
    endpoint.value = instance.backend.endpoint
    bucket.value = instance.backend.bucket
    region.value = instance.backend.region
    prefix.value = instance.backend.prefix
    addressingStyle.value = instance.backend.addressing_style
    capacityLimitGiB.value = bytesToGiB(instance.backend.capacity_limit_bytes)
  }
  editorOpen.value = true
}
function requestBody(): TestS3StorageRequest {
  if (provider.value === 'local') throw new Error('local storage does not use S3 credentials')
  return { provider: provider.value, endpoint: endpoint.value.trim(), bucket: bucket.value.trim(), region: region.value.trim(), prefix: prefix.value.trim(), addressing_style: addressingStyle.value, access_key_id: accessKeyId.value, secret_access_key: secretAccessKey.value, capacity_limit_bytes: capacityLimitBytes() }
}
async function run(action: () => Promise<void>, fallback: string): Promise<void> {
  if (busy.value) return
  busy.value = true
  errorMessage.value = ''
  try { await action() } catch (error) { errorMessage.value = error instanceof Error ? error.message : fallback } finally { busy.value = false }
}
async function testConnection(): Promise<void> {
  await run(async () => { await testS3Storage(requestBody()); emit('changed', locale.text('S3 连接验证通过', 'S3 connection verified')) }, locale.text('S3 连接测试失败', 'S3 connection test failed'))
}
async function saveStorage(): Promise<void> {
  await run(async () => {
    if (editingInstance.value) {
      if (editingInstance.value.backend.type === 'local') await updateLocalStorage(editingInstance.value.id, storageName.value.trim(), localPath.value.trim(), capacityLimitBytes())
      else await updateS3Storage(editingInstance.value.id, storageName.value.trim(), requestBody())
      await updateStorageAccess(editingInstance.value.id, enabled.value, allowGuestAccess.value)
      editorOpen.value = false
      emit('changed', locale.text('存储设置已保存', 'Storage settings saved'))
      return
    }
    const name = storageName.value.trim()
    if (!name) throw new Error(locale.text('请输入存储名称', 'Enter a storage name'))
    if (provider.value === 'local') {
      const path = localPath.value.trim()
      if (!path) throw new Error(locale.text('请输入本地存储路径', 'Enter a local storage path'))
      await addLocalStorage(path, name, capacityLimitBytes(), enabled.value, allowGuestAccess.value)
    } else {
      await stageS3Storage(name, requestBody(), enabled.value, allowGuestAccess.value)
      await activatePendingStorage()
    }
    editorOpen.value = false
    emit('changed', locale.text('存储源已添加；原路径中的文件会直接显示，文件未被移动或删除', 'Storage added. Existing files at the path are shown directly and were not moved or deleted.'))
  }, locale.text('无法保存存储源', 'Unable to save storage'))
}
async function removeInstance(instance: StorageInstanceView): Promise<void> {
  if (!window.confirm(locale.text(`删除存储配置“${instance.name}”？不会删除存储中的文件。`, `Remove “${instance.name}”? Stored files will not be deleted.`))) return
  await run(async () => { await deleteStorage(instance.id); emit('changed', locale.text('存储配置已删除，文件未被删除', 'Storage configuration removed; files were not deleted')) }, locale.text('无法删除存储配置；请先移除相关 WebDAV、文件夹锁或用户权限', 'Unable to remove storage; remove related WebDAV mounts, folder locks, or user permissions first'))
}
async function clearPending(): Promise<void> {
  await run(async () => { await discardPendingStorage(); emit('changed', locale.text('未完成的存储配置已清除', 'Pending storage configuration cleared')) }, locale.text('无法清除未完成配置', 'Unable to clear pending configuration'))
}
</script>

<template>
  <section class="admin-pane form-pane storage-pane glass" aria-labelledby="storage-title">
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
        <article v-for="instance in instances" :key="instance.id" class="storage-instance-card storage-source-row">
          <div class="storage-source-main">
            <div class="storage-source-title"><strong>{{ instance.name }}</strong><span class="status-pill" :class="{ warning: instance.status === 'abnormal', muted: instance.status === 'disabled' }">{{ statusLabel(instance) }}</span><span v-if="instance.allow_guest_access" class="status-pill">{{ locale.text('访客可访问', 'Guest access') }}</span></div>
            <p>{{ backendLabel(instance) }}<template v-if="instance.backend.type === 's3'"> · {{ instance.backend.bucket }} · {{ instance.backend.endpoint }}</template><template v-else> · {{ instance.backend.path }}</template></p>
            <small>{{ locale.text('已使用：', 'Used: ') }}{{ formatBytes(instance.usage_bytes) }} · {{ locale.text('预留：', 'Reserved: ') }}{{ formatBytes(instance.reserved_bytes) }} · {{ locale.text('上限：', 'Limit: ') }}{{ instance.backend.capacity_limit_bytes ? formatBytes(instance.backend.capacity_limit_bytes) : locale.text('未设置', 'Unlimited') }}</small>
          </div>
          <div class="storage-instance-actions"><button class="btn secondary" type="button" :disabled="busy" @click="openSettings(instance)">{{ locale.text('设置', 'Settings') }}</button><button class="btn danger" type="button" :disabled="busy" @click="removeInstance(instance)">{{ locale.text('删除', 'Remove') }}</button></div>
        </article>
      </div>
      <div v-else class="storage-empty storage-wide"><AppIcon name="storage" :size="40" /><strong>{{ locale.text('尚未添加存储源', 'No storage sources') }}</strong><span>{{ locale.text('点击右上角“新建存储”开始配置。', 'Use “New storage” to add one.') }}</span></div>
    </div>
  </section>

  <div v-if="editorOpen" class="overlay active" @click.self="editorOpen = false">
    <section class="modal storage-editor" role="dialog" aria-modal="true" aria-labelledby="storage-editor-title">
      <header class="storage-editor-head"><div><h2 id="storage-editor-title">{{ editingInstance ? locale.text('存储设置', 'Storage settings') : locale.text('新建存储', 'New storage') }}</h2><p>{{ editingInstance ? storageName : locale.text('先选择存储类型，再填写连接参数。', 'Choose a storage type, then enter its connection settings.') }}</p></div><button class="icon-btn flat" type="button" :aria-label="locale.t('common.close')" @click="editorOpen = false">×</button></header>
      <div v-if="!editingInstance" class="storage-provider-grid">
        <button v-for="item in providers" :key="item.id" type="button" class="storage-provider" :class="{ active: provider === item.id }" @click="selectProvider(item.id)"><strong>{{ locale.text(item.zh, item.en) }}</strong><small>{{ item.protocol }}</small></button>
      </div>
      <div class="storage-form">
        <label>{{ locale.text('存储名称', 'Storage name') }}<input v-model="storageName" class="input local-storage-name" maxlength="64"></label>
        <label v-if="provider === 'local'">{{ locale.text('本地存储路径', 'Local storage path') }}<input v-model="localPath" class="input local-storage-path" maxlength="4096" :placeholder="locale.text('例如 /mnt/data', 'For example /mnt/data')"><small>{{ locale.text('填写已通过系统或容器挂载并在部署配置中声明的目录。', 'Enter a directory mounted into Ycloud and declared by deployment configuration.') }}</small></label>
        <label v-if="provider === 'local'">{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input local-capacity-input" type="number" min="0" max="4194304" step="1"><small>{{ locale.text('0 表示不设置逻辑上限。', '0 disables the logical limit.') }}</small></label>
        <template v-if="isS3">
          <label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}<input v-model="endpoint" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com"></label><label>Bucket<input v-model="bucket" class="input" maxlength="63"></label><label>Region<input v-model="region" class="input" maxlength="64"></label><label class="storage-wide">Prefix<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/"></label><label>Access Key ID<input v-model="accessKeyId" class="input" maxlength="256" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>Secret Access Key<input v-model="secretAccessKey" class="input" type="password" maxlength="4096" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>{{ locale.text('寻址方式', 'Addressing style') }}<select v-model="addressingStyle" class="input" :disabled="isOfficialCloud"><option value="path">Path Style</option><option value="virtual_hosted">Virtual Hosted</option></select></label><label>{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" max="4194304" step="1"></label>
        </template>
        <div class="storage-access-options storage-wide"><label class="storage-access-option"><input v-model="enabled" type="checkbox"><span><strong>{{ locale.text('启动', 'Enabled') }}</strong><small>{{ locale.text('停用后不会出现在文件页切换器中。', 'Disabled storage is hidden from the file browser.') }}</small></span></label><label class="storage-access-option"><input v-model="allowGuestAccess" type="checkbox"><span><strong>{{ locale.text('允许访客访问', 'Allow guest access') }}</strong><small>{{ locale.text('关闭后需要管理员或已授权用户账号。', 'When off, an administrator or authorized user must sign in.') }}</small></span></label></div>
        <div v-if="provider === 'local' && localPath.trim()" class="storage-boundary-note storage-wide">{{ locale.text('保存后直接展示该路径中的现有文件，不进行导入、移动或删除。', 'Existing files at this path are shown directly; nothing is imported, moved, or deleted.') }}</div>
        <p class="admin-form-error storage-wide">{{ errorMessage }}</p>
      </div>
      <div class="modal-actions"><button v-if="!editingInstance && isS3" class="btn secondary" type="button" :disabled="busy" @click="testConnection">{{ locale.text('测试连接', 'Test connection') }}</button><button class="btn secondary" type="button" :disabled="busy" @click="editorOpen = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="button" :disabled="busy" @click="saveStorage">{{ locale.t('common.save') }}</button></div>
    </section>
  </div>
</template>

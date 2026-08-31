<script setup lang="ts">
import { computed, ref } from 'vue'
import type { LocalMountView, S3AddressingStyle, S3Provider, StorageInstanceView, TestS3StorageRequest } from '../../shared/api/admin'
import { activatePendingStorage, addLocalStorage, deleteStorage, discardPendingStorage, setDefaultStorage, stageS3Storage, testLocalStorage, testS3Storage, updateLocalStorage, updateS3Storage } from '../../shared/api/admin'
import AppIcon from '../../shared/components/AppIcon.vue'
import AppSelect from '../../shared/components/AppSelect.vue'
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
const localMountOptions = computed(() => props.localMounts
  .filter(mount => !mount.storage_id || mount.storage_id === editingId.value)
  .map(mount => ({
    value: mount.path,
    label: `${mount.name} · ${mount.path}${mount.ready ? '' : locale.text('（不可用）', ' (unavailable)')}`,
    disabled: !mount.ready,
  })))
const isS3 = computed(() => provider.value !== 'local')
const isOfficialCloud = computed(() => provider.value === 'alibaba_oss' || provider.value === 'tencent_cos')
const addressingOptions = [
  { value: 'path', label: 'Path Style' },
  { value: 'virtual_hosted', label: 'Virtual Hosted' },
]

function bytesToGiB(value: number | null): number { return value ? value / (1024 ** 3) : 0 }
function formatBytes(value: number): string {
  if (value >= 1024 ** 4) return `${(value / (1024 ** 4)).toFixed(2)} TiB`
  if (value >= 1024 ** 3) return `${(value / (1024 ** 3)).toFixed(2)} GiB`
  if (value >= 1024 ** 2) return `${(value / (1024 ** 2)).toFixed(2)} MiB`
  return `${value} B`
}
function statusLabel(instance: StorageInstanceView): string {
  if (instance.status === 'disabled') return locale.text('停用', 'Disabled')
  if (instance.status === 'abnormal') return locale.text('异常', 'Abnormal')
  return locale.text('启用', 'Enabled')
}
function capacityLimitBytes(): number | null {
  const value = Number(capacityLimitGiB.value)
  if (!Number.isFinite(value) || value < 0 || value > 4_194_304) throw new Error(locale.text('容量上限必须在 0 到 4194304 GiB 之间', 'Capacity must be from 0 to 4194304 GiB'))
  if (value === 0) return null
  const bytes = Math.round(value * (1024 ** 3))
  if (bytes < 1024 ** 2) throw new Error(locale.text('容量上限不能小于 1 MiB', 'Capacity cannot be less than 1 MiB'))
  return bytes
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
function openNew(): void { resetEditor(); localPath.value = String(localMountOptions.value[0]?.value ?? ''); editorOpen.value = true }
function selectProvider(value: StorageOption): void {
  provider.value = value
  addressingStyle.value = value === 'alibaba_oss' || value === 'tencent_cos' ? 'virtual_hosted' : 'path'
  if (value === 'local' && !localPath.value) localPath.value = String(localMountOptions.value[0]?.value ?? '')
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
  await run(async () => {
    if (provider.value === 'local') await testLocalStorage(localPath.value.trim())
    else await testS3Storage(requestBody())
    emit('changed', locale.text('存储连接与读写能力验证通过', 'Storage connectivity and read/write capabilities verified'))
  }, locale.text('存储连接测试失败', 'Storage connection test failed'))
}
async function saveStorage(): Promise<void> {
  await run(async () => {
    if (editingInstance.value) {
      if (editingInstance.value.backend.type === 'local') await updateLocalStorage(editingInstance.value.id, storageName.value.trim(), localPath.value.trim(), capacityLimitBytes(), enabled.value, allowGuestAccess.value)
      else await updateS3Storage(editingInstance.value.id, storageName.value.trim(), requestBody(), enabled.value, allowGuestAccess.value)
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
async function makeDefault(instance: StorageInstanceView): Promise<void> {
  await run(async () => { await setDefaultStorage(instance.id); emit('changed', locale.text('默认存储已更新', 'Default storage updated')) }, locale.text('无法设置默认存储', 'Unable to set default storage'))
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
          <strong class="admin-record-name">{{ instance.name }}</strong>
          <div class="admin-record-value">
            <span>{{ locale.text('存储用量', 'Storage used') }}</span>
            <strong v-if="instance.capacity_accurate !== false">{{ formatBytes(instance.usage_bytes) }}</strong>
            <strong v-else>{{ locale.text('核对中', 'Checking') }}</strong>
          </div>
          <div class="admin-record-end">
            <div class="admin-record-status"><span class="status-pill" :class="{ warning: instance.status === 'abnormal', muted: instance.status === 'disabled' }">{{ statusLabel(instance) }}</span><span v-if="instance.is_default" class="status-pill">{{ locale.text('默认', 'Default') }}</span><span v-if="instance.allow_guest_access" class="status-pill">{{ locale.text('访客可访问', 'Guest access') }}</span></div>
            <div class="storage-instance-actions">
              <button v-if="!instance.is_default" class="record-icon-btn" type="button" :disabled="busy || !instance.ready" :title="locale.text('设为默认存储', 'Set as default storage')" :aria-label="locale.text('设为默认存储', 'Set as default storage')" @click="makeDefault(instance)"><AppIcon name="star" :size="18" /></button>
              <button class="record-icon-btn" type="button" :disabled="busy" :title="locale.text('编辑存储', 'Edit storage')" :aria-label="locale.text('编辑存储', 'Edit storage')" @click="openSettings(instance)"><AppIcon name="rename" :size="18" /></button>
              <button class="record-icon-btn danger" type="button" :disabled="busy || instance.is_default" :title="instance.is_default ? locale.text('默认存储不能删除', 'The default storage cannot be deleted') : locale.text('删除存储', 'Delete storage')" :aria-label="locale.text('删除存储', 'Delete storage')" @click="removeInstance(instance)"><AppIcon name="delete" :size="18" /></button>
            </div>
          </div>
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
        <label :class="{ 'storage-wide': provider === 'local' }">{{ locale.text('存储名称', 'Storage name') }}<input v-model="storageName" class="input local-storage-name" maxlength="64"></label>
        <label v-if="provider === 'local'">{{ locale.text('本地存储挂载', 'Local storage mount') }}<AppSelect v-model="localPath" class="local-storage-path" :options="localMountOptions" :label="locale.text('本地存储挂载', 'Local storage mount')" /><small>{{ locale.text('这里只显示启动参数中已声明且尚未使用的挂载点。', 'Only declared and unused deployment mounts are shown.') }}</small></label>
        <label v-if="provider === 'local'">{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input local-capacity-input" type="number" min="0" max="4194304" step="0.001"><small>{{ locale.text('0 表示不设置逻辑上限，最小 1 MiB。', '0 disables the logical limit; minimum 1 MiB.') }}</small></label>
        <template v-if="isS3">
          <label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}<input v-model="endpoint" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com"><small v-if="provider === 'minio' || provider === 's3_compatible'">{{ locale.text('自定义 Endpoint 必须同时加入启动参数 S3_ALLOWED_ENDPOINTS。', 'Custom endpoints must also be listed in S3_ALLOWED_ENDPOINTS at startup.') }}</small></label><label>Bucket<input v-model="bucket" class="input" maxlength="63"></label><label>Region<input v-model="region" class="input" maxlength="64"></label><label class="storage-wide">Prefix<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/"></label><label>Access Key ID<input v-model="accessKeyId" class="input" maxlength="256" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>Secret Access Key<input v-model="secretAccessKey" class="input" type="password" maxlength="4096" :placeholder="editingInstance ? locale.text('留空则保留原密钥', 'Leave blank to keep the current key') : ''"></label><label>{{ locale.text('寻址方式', 'Addressing style') }}<AppSelect v-model="addressingStyle" :options="addressingOptions" :label="locale.text('寻址方式', 'Addressing style')" :disabled="isOfficialCloud" /></label><label>{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" max="4194304" step="0.001"></label>
        </template>
        <div class="storage-access-options storage-wide"><label class="storage-access-option"><input v-model="enabled" type="checkbox"><span><strong>{{ locale.text('启动', 'Enabled') }}</strong><small>{{ locale.text('停用后不会出现在文件页切换器中。', 'Disabled storage is hidden from the file browser.') }}</small></span></label><label class="storage-access-option"><input v-model="allowGuestAccess" type="checkbox"><span><strong>{{ locale.text('允许访客访问', 'Allow guest access') }}</strong><small>{{ locale.text('关闭后需要管理员或已授权用户账号。', 'When off, an administrator or authorized user must sign in.') }}</small></span></label></div>
        <div v-if="provider === 'local' && localPath.trim()" class="storage-boundary-note storage-wide">{{ locale.text('保存后直接展示该路径中的现有文件，不进行导入、移动或删除。', 'Existing files at this path are shown directly; nothing is imported, moved, or deleted.') }}</div>
        <p class="admin-form-error storage-wide">{{ errorMessage }}</p>
      </div>
      <div class="modal-actions"><button v-if="!editingInstance" class="btn secondary" type="button" :disabled="busy" @click="testConnection">{{ locale.text('测试连接', 'Test connection') }}</button><button class="btn secondary" type="button" :disabled="busy" @click="editorOpen = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="button" :disabled="busy" @click="saveStorage">{{ locale.t('common.save') }}</button></div>
    </section>
  </div>
</template>

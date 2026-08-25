<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { LocalMountView, S3AddressingStyle, S3Provider, StorageInstanceView, TestS3StorageRequest } from '../../shared/api/admin'
import { activatePendingStorage, addLocalStorage, deleteStorage, discardPendingStorage, setDefaultStorage, stageS3Storage, testS3Storage, updateLocalStorage } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ instances: StorageInstanceView[]; pendingInstance: StorageInstanceView | null; defaultStorageId: string; localMounts: LocalMountView[] }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
type StorageOption = 'local' | S3Provider

const provider = ref<StorageOption>('local')
const storageName = ref('')
const localStorageName = ref('')
const selectedLocalMountId = ref('')
const endpoint = ref('')
const bucket = ref('')
const region = ref('us-east-1')
const prefix = ref('')
const addressingStyle = ref<S3AddressingStyle>('path')
const accessKeyId = ref('')
const secretAccessKey = ref('')
const capacityLimitGiB = ref(0)
const busy = ref(false)
const errorMessage = ref('')
const confirmingActivation = ref(false)

const providers: Array<{ id: StorageOption; zh: string; en: string; protocol: string }> = [
  { id: 'local', zh: '本地存储', en: 'Local storage', protocol: 'Filesystem' },
  { id: 'alibaba_oss', zh: '阿里云 OSS', en: 'Alibaba Cloud OSS', protocol: 'S3 SigV4' },
  { id: 'tencent_cos', zh: '腾讯云 COS', en: 'Tencent Cloud COS', protocol: 'S3 SigV4' },
  { id: 'minio', zh: 'MinIO / RustFS', en: 'MinIO / RustFS', protocol: 'S3 SigV4' },
  { id: 's3_compatible', zh: 'S3 通用协议', en: 'Generic S3-compatible', protocol: 'S3 SigV4' },
]

const localMounts = computed<LocalMountView[]>(() => props.localMounts.length ? props.localMounts : props.instances.flatMap(instance => instance.backend.type === 'local' ? [{
  mount_id: instance.backend.mount_id ?? instance.id,
  name: instance.name,
  path: instance.backend.path,
  storage_id: instance.id,
  ready: instance.ready,
  total_bytes: null,
  available_bytes: null,
}] : []))
const selectedLocalMount = computed(() => localMounts.value.find(item => item.mount_id === selectedLocalMountId.value))
const selectedLocalInstance = computed(() => {
  const storageId = selectedLocalMount.value?.storage_id
  return storageId ? props.instances.find(item => item.id === storageId) : undefined
})
const addedLocalMountCount = computed(() => localMounts.value.filter(item => item.storage_id).length)
const isS3 = computed(() => provider.value !== 'local')
const isOfficialCloud = computed(() => provider.value === 'alibaba_oss' || provider.value === 'tencent_cos')
const localLimitChanged = computed(() => {
  const value = Number(capacityLimitGiB.value)
  const selected = value === 0 ? null : value * (1024 ** 3)
  return Number.isInteger(value) && value >= 0 && selectedLocalInstance.value?.backend.type === 'local' && selectedLocalInstance.value.backend.capacity_limit_bytes !== selected
})

watch(provider, value => {
  if (value === 'local') {
    const mount = selectedLocalMount.value ?? localMounts.value[0]
    if (mount) selectLocalMount(mount)
  } else {
    capacityLimitGiB.value = 0
    addressingStyle.value = value === 'alibaba_oss' || value === 'tencent_cos' ? 'virtual_hosted' : 'path'
  }
})
watch(() => [props.localMounts, props.instances], () => {
  const selected = localMounts.value.find(item => item.mount_id === selectedLocalMountId.value) ?? localMounts.value[0]
  if (selected && provider.value === 'local') selectLocalMount(selected)
}, { immediate: true, deep: true })

function bytesToGiB(value: number | null): number { return value ? value / (1024 ** 3) : 0 }
function formatBytes(value: number): string {
  if (value >= 1024 ** 4) return `${(value / (1024 ** 4)).toFixed(2)} TiB`
  if (value >= 1024 ** 3) return `${(value / (1024 ** 3)).toFixed(2)} GiB`
  if (value >= 1024 ** 2) return `${(value / (1024 ** 2)).toFixed(2)} MiB`
  return `${value} B`
}
function formatOptionalBytes(value: number | null): string {
  return value === null ? locale.text('未知', 'Unknown') : formatBytes(value)
}
function selectLocalMount(mount: LocalMountView): void {
  selectedLocalMountId.value = mount.mount_id
  const instance = mount.storage_id ? props.instances.find(item => item.id === mount.storage_id) : undefined
  localStorageName.value = instance?.name ?? mount.name
  capacityLimitGiB.value = bytesToGiB(instance?.backend.type === 'local' ? instance.backend.capacity_limit_bytes : null)
  errorMessage.value = ''
}
function mountForInstance(instance: StorageInstanceView): LocalMountView | undefined {
  return localMounts.value.find(item => item.storage_id === instance.id)
}
function providerLabel(value: S3Provider): string {
  const item = providers.find(candidate => candidate.id === value)
  return item ? locale.text(item.zh, item.en) : 'S3'
}
function backendLabel(instance: StorageInstanceView): string {
  return instance.backend.type === 'local' ? locale.text('本地存储', 'Local storage') : providerLabel(instance.backend.provider)
}
function capacityLimitBytes(): number | null {
  const value = Number(capacityLimitGiB.value)
  if (!Number.isInteger(value) || value < 0 || value > 4_194_304) throw new Error(locale.text('容量上限必须是 0 到 4194304 之间的整数 GiB', 'Capacity must be an integer from 0 to 4194304 GiB'))
  return value === 0 ? null : value * (1024 ** 3)
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
  await run(async () => { await testS3Storage(requestBody()); emit('changed', locale.text('S3 连接验证通过；配置尚未保存', 'S3 connection verified; configuration was not saved')) }, locale.text('S3 连接测试失败', 'S3 connection test failed'))
}
async function stageS3(): Promise<void> {
  const name = storageName.value.trim()
  if (!name) { errorMessage.value = locale.text('请输入存储名称', 'Enter a storage name'); return }
  await run(async () => { await stageS3Storage(name, requestBody()); accessKeyId.value = ''; secretAccessKey.value = ''; emit('changed', locale.text('S3 完整能力验证通过，已保存为待添加存储', 'S3 capability checks passed; the storage is ready to be added')) }, locale.text('无法保存待添加存储', 'Unable to save the pending storage'))
}
async function saveLocal(): Promise<void> {
  const instance = selectedLocalInstance.value
  if (!instance) return
  await run(async () => { await updateLocalStorage(instance.id, capacityLimitBytes()); emit('changed', locale.text('本地存储容量设置已更新', 'Local storage capacity updated')) }, locale.text('无法保存本地存储配置', 'Unable to save local storage settings'))
}
async function addSelectedLocal(): Promise<void> {
  const mount = selectedLocalMount.value
  const name = localStorageName.value.trim()
  if (!mount || mount.storage_id || !mount.ready) return
  if (!name) { errorMessage.value = locale.text('请输入存储名称', 'Enter a storage name'); return }
  await run(async () => {
    await addLocalStorage(mount.mount_id, name, capacityLimitBytes())
    emit('changed', locale.text('本地存储源已添加；现有文件和默认存储均未改变', 'Local storage added; existing files and the default storage were not changed'))
  }, locale.text('无法添加本地存储源', 'Unable to add local storage'))
}
async function discardPending(): Promise<void> {
  await run(async () => { await discardPendingStorage(); emit('changed', locale.text('待添加存储配置已删除', 'Pending storage configuration removed')) }, locale.text('无法删除待添加配置', 'Unable to remove the pending configuration'))
}
async function activatePending(): Promise<void> {
  await run(async () => { await activatePendingStorage(); confirmingActivation.value = false; emit('changed', locale.text('新存储已添加；默认存储和现有文件均未改变', 'Storage added; the default storage and existing files were not changed')) }, locale.text('存储添加失败', 'Unable to add storage'))
}
async function makeDefault(id: string): Promise<void> {
  await run(async () => { await setDefaultStorage(id); emit('changed', locale.text('默认展示存储已更新，旧网页访问令牌已撤销', 'Default storage updated; previous browser tokens were revoked')) }, locale.text('无法切换默认存储', 'Unable to change default storage'))
}
async function removeInstance(instance: StorageInstanceView): Promise<void> {
  if (!window.confirm(locale.text(`删除存储配置“${instance.name}”？不会删除存储中的文件。`, `Remove “${instance.name}”? Stored files will not be deleted.`))) return
  await run(async () => { await deleteStorage(instance.id); emit('changed', locale.text('存储配置已删除，文件未被删除', 'Storage configuration removed; files were not deleted')) }, locale.text('无法删除存储配置', 'Unable to remove storage'))
}
</script>

<template>
  <section class="admin-pane form-pane storage-pane glass" aria-labelledby="storage-title">
    <header class="admin-pane-head"><div><h1 id="storage-title">{{ locale.text('存储设置', 'Storage') }}</h1><p>{{ locale.text('每个存储拥有独立命名空间；配置操作不会迁移或删除文件。', 'Each storage has an independent namespace. Configuration changes never migrate or delete files.') }}</p></div></header>
    <div class="admin-pane-body storage-body">
      <div class="storage-instance-list storage-wide">
        <article v-for="instance in instances" :key="instance.id" class="storage-instance-card">
          <div><strong>{{ instance.name }}</strong> <span v-if="instance.is_default" class="status-pill">{{ locale.text('默认', 'Default') }}</span> <span class="status-pill" :class="{ warning: !instance.ready }">{{ instance.ready ? locale.text('可用', 'Ready') : locale.text('不可用', 'Unavailable') }}</span><p>{{ backendLabel(instance) }}<template v-if="instance.backend.type === 's3'"> · {{ instance.backend.bucket }} · {{ instance.backend.endpoint }}</template><template v-else> · {{ instance.backend.path }}</template></p><small>{{ locale.text('已使用：', 'Used: ') }}{{ formatBytes(instance.usage_bytes) }} · {{ locale.text('预留：', 'Reserved: ') }}{{ formatBytes(instance.reserved_bytes) }} · {{ locale.text('上限：', 'Limit: ') }}{{ instance.backend.capacity_limit_bytes ? formatBytes(instance.backend.capacity_limit_bytes) : locale.text('未设置', 'Unlimited') }}<template v-if="instance.backend.type === 'local' && mountForInstance(instance)"> · {{ locale.text('磁盘可用：', 'Disk available: ') }}{{ formatOptionalBytes(mountForInstance(instance)?.available_bytes ?? null) }}</template></small></div>
          <div class="storage-instance-actions"><button v-if="!instance.is_default" class="btn secondary" type="button" :disabled="busy || !instance.ready" @click="makeDefault(instance.id)">{{ locale.text('设为默认', 'Make default') }}</button><button v-if="!instance.is_default" class="btn danger" type="button" :disabled="busy" @click="removeInstance(instance)">{{ locale.text('删除配置', 'Remove') }}</button></div>
        </article>
      </div>
      <div class="storage-provider-grid storage-wide"><button v-for="item in providers" :key="item.id" type="button" class="storage-provider" :class="{ active: provider === item.id }" @click="provider = item.id; errorMessage = ''"><strong>{{ locale.text(item.zh, item.en) }}</strong><small>{{ item.protocol }}</small></button></div>
      <div v-if="pendingInstance" class="storage-pending-card storage-wide"><div><strong>{{ locale.text('待添加：', 'Pending: ') }}{{ pendingInstance.name }}</strong><p v-if="pendingInstance.backend.type === 's3'">{{ providerLabel(pendingInstance.backend.provider) }} · {{ pendingInstance.backend.bucket }} · {{ pendingInstance.backend.endpoint }}</p><small>{{ locale.text('尚未进入可选存储列表；凭据不会返回浏览器。', 'It is not selectable yet; credentials are never returned to the browser.') }}</small></div><div class="storage-pending-actions"><button class="btn secondary" type="button" :disabled="busy" @click="discardPending">{{ locale.text('删除', 'Discard') }}</button><button class="btn" type="button" :disabled="busy" @click="confirmingActivation = true">{{ locale.text('添加存储', 'Add storage') }}</button></div></div>
      <div v-if="!isS3" class="storage-form local-storage-form">
        <div class="local-mount-summary storage-wide"><strong>{{ locale.text(`已声明 ${localMounts.length} 个挂载地址，已添加 ${addedLocalMountCount} 个存储源。`, `${localMounts.length} mount(s) declared; ${addedLocalMountCount} storage source(s) added.`) }}</strong><small>{{ locale.text('地址由部署配置提供，后台不能填写任意服务器路径。', 'Mount addresses come from deployment configuration; arbitrary server paths cannot be entered here.') }}</small></div>
        <div class="local-mount-grid storage-wide">
          <button v-for="mount in localMounts" :key="mount.mount_id" :data-mount-id="mount.mount_id" class="local-mount-card" :class="{ active: selectedLocalMountId === mount.mount_id }" type="button" @click="selectLocalMount(mount)">
            <span><strong>{{ mount.name }}</strong><span v-if="mount.storage_id" class="status-pill">{{ locale.text('已添加', 'Added') }}</span><span class="status-pill" :class="{ warning: !mount.ready }">{{ mount.ready ? locale.text('可用', 'Ready') : locale.text('不可用', 'Unavailable') }}</span></span>
            <code>{{ mount.path }}</code>
            <small>{{ locale.text('总容量：', 'Total: ') }}{{ formatOptionalBytes(mount.total_bytes) }} · {{ locale.text('可用：', 'Available: ') }}{{ formatOptionalBytes(mount.available_bytes) }}</small>
          </button>
        </div>
        <template v-if="selectedLocalMount">
          <label class="storage-wide">{{ locale.text('部署挂载地址', 'Deployment mount address') }}<input class="input local-path-input" :value="selectedLocalMount.path" readonly></label>
          <label v-if="!selectedLocalMount.storage_id">{{ locale.text('存储名称', 'Storage name') }}<input v-model="localStorageName" class="input local-storage-name" maxlength="64"></label>
          <label>{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input local-capacity-input" type="number" min="0" max="4194304" step="1"><small>{{ locale.text('0 表示不设置逻辑上限；磁盘安全余量仍固定保留。', '0 disables the logical limit; the disk reserve remains fixed.') }}</small></label>
          <div class="storage-boundary-note storage-wide">{{ locale.text('主目录由 STORAGE_PATH 提供；其他地址由 LOCAL_STORAGE_MOUNTS 与 Compose 卷共同声明。网页只添加声明过的地址和调整逻辑容量。', 'STORAGE_PATH provides the primary directory. LOCAL_STORAGE_MOUNTS and Compose volumes declare additional addresses. The UI only adds declared addresses and adjusts logical capacity.') }}</div>
          <p class="admin-form-error storage-wide">{{ errorMessage }}</p>
          <div class="admin-save-row storage-wide">
            <button v-if="selectedLocalMount.storage_id && localLimitChanged" class="btn local-save-button" type="button" :disabled="busy" @click="saveLocal">{{ locale.t('common.save') }}</button>
            <button v-else-if="!selectedLocalMount.storage_id" class="btn local-add-button" type="button" :disabled="busy || !selectedLocalMount.ready" @click="addSelectedLocal">{{ locale.text('添加为存储源', 'Add as storage source') }}</button>
          </div>
        </template>
        <div v-else class="storage-boundary-note storage-wide">{{ locale.text('当前部署没有可用的本地挂载地址。', 'This deployment has no local mount addresses available.') }}</div>
      </div>
      <form v-else class="storage-form" @submit.prevent="stageS3"><label>{{ locale.text('存储名称', 'Storage name') }}<input v-model="storageName" class="input" maxlength="64"></label><label>{{ locale.text('容量上限（GiB）', 'Capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" max="4194304"></label><label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}<input v-model="endpoint" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com"></label><label>Bucket<input v-model="bucket" class="input" maxlength="63"></label><label>Region<input v-model="region" class="input" maxlength="64"></label><label class="storage-wide">Prefix<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/"></label><label>Access Key ID<input v-model="accessKeyId" class="input" maxlength="256"></label><label>Secret Access Key<input v-model="secretAccessKey" class="input" type="password" maxlength="4096"></label><label>{{ locale.text('寻址方式', 'Addressing style') }}<select v-model="addressingStyle" class="input" :disabled="isOfficialCloud"><option value="path">Path Style</option><option value="virtual_hosted">Virtual Hosted</option></select></label><div class="storage-boundary-note">{{ locale.text('保存前只在保留区验证列举、写入、读取、复制和删除，不触碰用户对象。', 'Before saving, Ycloud validates list, write, read, copy, and delete only in its reserved area.') }}</div><p class="admin-form-error storage-wide">{{ errorMessage }}</p><div class="admin-save-row storage-wide storage-form-actions"><button class="btn secondary" type="button" :disabled="busy" @click="testConnection">{{ locale.text('仅测试连接', 'Test only') }}</button><button class="btn" type="submit" :disabled="busy">{{ locale.text('验证并保存', 'Verify and save') }}</button></div></form>
    </div>
  </section>
  <div v-if="confirmingActivation && pendingInstance" class="overlay active" @click.self="confirmingActivation = false"><section class="modal-card" role="dialog" aria-modal="true"><h2>{{ locale.text('添加存储？', 'Add storage?') }}</h2><p>{{ locale.text('重新验证后加入可选存储列表；默认展示存储不会改变。', 'After revalidation, the storage is added to the selectable list. The default storage does not change.') }}</p><div class="modal-actions"><button class="btn secondary" type="button" @click="confirmingActivation = false">{{ locale.t('common.cancel') }}</button><button class="btn" type="button" :disabled="busy" @click="activatePending">{{ locale.text('确认添加', 'Add storage') }}</button></div></section></div>
</template>

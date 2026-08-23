<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { S3AddressingStyle, S3Provider, StorageBackendView, TestS3StorageRequest } from '../../shared/api/admin'
import { activatePendingStorage, discardPendingStorage, stageLocalStorage, stageS3Storage, testS3Storage } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{
  backend: StorageBackendView
  pendingBackend: StorageBackendView | null
  localPath: string
  usageBytes: number
  reservedBytes: number
}>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
type StorageOption = 'local' | S3Provider

const provider = ref<StorageOption>(props.backend.type === 'local' ? 'local' : props.backend.provider)
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

const isS3 = computed(() => provider.value !== 'local')
const isOfficialCloud = computed(() => provider.value === 'alibaba_oss' || provider.value === 'tencent_cos')
const activeLabel = computed(() => props.backend.type === 'local' ? locale.text('本地存储', 'Local storage') : providerLabel(props.backend.provider))
const localLimitChanged = computed(() => {
  const value = Number(capacityLimitGiB.value)
  if (!Number.isInteger(value) || value < 0 || value > 4_194_304) return true
  const selected = value === 0 ? null : value * (1024 ** 3)
  return props.backend.type !== 'local' || props.backend.capacity_limit_bytes !== selected
})

watch(provider, value => {
  if (value === 'local') return
  addressingStyle.value = value === 'alibaba_oss' || value === 'tencent_cos' ? 'virtual_hosted' : 'path'
})

watch(() => [props.backend, props.pendingBackend] as const, ([active, pending]) => {
  const source = pending?.type === 's3' ? pending : active.type === 's3' ? active : undefined
  if (!source || source.type !== 's3') return
  provider.value = source.provider
  endpoint.value = source.endpoint
  bucket.value = source.bucket
  region.value = source.region
  prefix.value = source.prefix
  addressingStyle.value = source.addressing_style
  capacityLimitGiB.value = bytesToGiB(source.capacity_limit_bytes)
}, { immediate: true })

watch(provider, value => {
  const configured = props.pendingBackend?.type === 'local'
    ? props.pendingBackend
    : props.backend.type === 'local' ? props.backend : undefined
  if (value === 'local') capacityLimitGiB.value = bytesToGiB(configured?.capacity_limit_bytes ?? null)
}, { immediate: true })

function providerLabel(value: S3Provider): string {
  const item = providers.find(providerItem => providerItem.id === value)
  return item ? locale.text(item.zh, item.en) : 'S3'
}

function bytesToGiB(value: number | null): number {
  return value ? value / (1024 ** 3) : 0
}

function capacityLimitBytes(): number | null {
  const value = Number(capacityLimitGiB.value)
  if (!Number.isInteger(value) || value < 0 || value > 4_194_304) {
    throw new Error(locale.text('容量上限必须是 0 到 4194304 之间的整数 GiB', 'Capacity must be an integer from 0 to 4194304 GiB'))
  }
  return value === 0 ? null : value * (1024 ** 3)
}

function formatBytes(value: number): string {
  if (value >= 1024 ** 4) return `${(value / (1024 ** 4)).toFixed(2)} TiB`
  if (value >= 1024 ** 3) return `${(value / (1024 ** 3)).toFixed(2)} GiB`
  if (value >= 1024 ** 2) return `${(value / (1024 ** 2)).toFixed(2)} MiB`
  return `${value} B`
}

function requestBody(): TestS3StorageRequest {
  if (provider.value === 'local') throw new Error('local storage does not use S3 credentials')
  return {
    provider: provider.value,
    endpoint: endpoint.value.trim(),
    bucket: bucket.value.trim(),
    region: region.value.trim(),
    prefix: prefix.value.trim(),
    addressing_style: addressingStyle.value,
    access_key_id: accessKeyId.value,
    secret_access_key: secretAccessKey.value,
    capacity_limit_bytes: capacityLimitBytes(),
  }
}

function clearSecrets(): void {
  accessKeyId.value = ''
  secretAccessKey.value = ''
}

async function run(action: () => Promise<void>, fallback: string): Promise<void> {
  if (busy.value) return
  busy.value = true
  errorMessage.value = ''
  try {
    await action()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : fallback
  } finally {
    busy.value = false
  }
}

async function testConnection(): Promise<void> {
  await run(async () => {
    await testS3Storage(requestBody())
    clearSecrets()
    emit('changed', locale.text('S3 连接与最小列举权限验证通过；配置尚未保存', 'S3 connection and minimum list permission verified; configuration was not saved'))
  }, locale.text('S3 连接测试失败', 'S3 connection test failed'))
}

async function stageS3(): Promise<void> {
  await run(async () => {
    await stageS3Storage(requestBody())
    clearSecrets()
    emit('changed', locale.text('S3 读、写、复制和删除验证通过，已保存为待启用配置', 'S3 read, write, copy, and delete checks passed; saved as pending'))
  }, locale.text('无法保存待启用存储', 'Unable to save pending storage'))
}

async function stageLocal(): Promise<void> {
  await run(async () => {
    await stageLocalStorage(capacityLimitBytes())
    emit('changed', locale.text('本地存储已保存为待启用配置', 'Local storage was saved as pending'))
  }, locale.text('无法保存本地存储配置', 'Unable to save local storage configuration'))
}

async function discardPending(): Promise<void> {
  await run(async () => {
    await discardPendingStorage()
    emit('changed', locale.text('待启用存储配置已删除，当前存储未改变', 'Pending storage configuration removed; active storage was unchanged'))
  }, locale.text('无法删除待启用配置', 'Unable to remove pending configuration'))
}

async function activatePending(): Promise<void> {
  if (!props.pendingBackend) return
  await run(async () => {
    await activatePendingStorage()
    confirmingActivation.value = false
    emit('changed', locale.text('存储后端已安全切换；现有文件没有迁移或删除', 'Storage backend switched safely; existing files were not migrated or deleted'))
  }, locale.text('存储切换失败，当前存储保持不变', 'Storage activation failed; the current backend remains active'))
}
</script>

<template>
  <section class="admin-pane form-pane storage-pane glass" aria-labelledby="storage-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="storage-title">{{ locale.text('存储设置', 'Storage') }}</h1>
        <p>{{ locale.text('先验证并保存为待启用配置，再单独确认切换。切换不会迁移或删除任何文件。', 'Verify and save a pending backend first, then activate it separately. Switching never migrates or deletes files.') }}</p>
      </div>
      <span class="status-pill">{{ locale.text('当前：', 'Active: ') }}{{ activeLabel }}</span>
    </header>
    <div class="admin-pane-body storage-body">
      <div class="storage-provider-grid" role="radiogroup" :aria-label="locale.text('存储类型', 'Storage backend')">
        <button v-for="item in providers" :key="item.id" type="button" class="storage-provider" :class="{ active: provider === item.id }" role="radio" :aria-checked="provider === item.id" @click="provider = item.id; errorMessage = ''">
          <strong>{{ locale.text(item.zh, item.en) }}</strong><small>{{ item.protocol }}</small>
        </button>
      </div>
      <div class="storage-boundary-note storage-wide">
        {{ locale.text('当前用户数据：', 'Current user data: ') }}{{ formatBytes(usageBytes) }}
        · {{ locale.text('活动写入预留：', 'In-flight reservation: ') }}{{ formatBytes(reservedBytes) }}
        · {{ locale.text('逻辑容量上限：', 'Logical limit: ') }}{{ backend.capacity_limit_bytes ? formatBytes(backend.capacity_limit_bytes) : locale.text('未设置', 'Unlimited') }}
      </div>

      <div v-if="pendingBackend" class="storage-pending-card">
        <div>
          <strong>{{ locale.text('待启用配置', 'Pending backend') }}</strong>
          <p v-if="pendingBackend.type === 'local'">{{ locale.text('本地存储', 'Local storage') }} · {{ localPath }}</p>
          <p v-else>{{ providerLabel(pendingBackend.provider) }} · {{ pendingBackend.bucket }} · {{ pendingBackend.endpoint }}</p>
          <small>{{ locale.text('容量上限：', 'Capacity: ') }}{{ pendingBackend.capacity_limit_bytes ? formatBytes(pendingBackend.capacity_limit_bytes) : locale.text('未设置', 'Unlimited') }} · {{ locale.text('尚未接管任何用户流量；凭据不会返回浏览器。', 'It is not serving user traffic; credentials are never returned to the browser.') }}</small>
        </div>
        <div class="storage-pending-actions">
          <button class="btn secondary" type="button" :disabled="busy" @click="discardPending">{{ locale.text('删除待启用配置', 'Discard') }}</button>
          <button class="btn" type="button" :disabled="busy" @click="confirmingActivation = true">{{ locale.text('启用', 'Activate') }}</button>
        </div>
      </div>

      <div v-if="!isS3" class="storage-form storage-local-form">
        <label class="storage-wide">{{ locale.text('本地存储目录', 'Local storage directory') }}<input class="input" :value="localPath" readonly aria-readonly="true"></label>
        <label>{{ locale.text('Ycloud 容量上限（GiB）', 'Ycloud capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" max="4194304" step="1"><small>{{ locale.text('0 表示不设置逻辑上限；磁盘安全余量仍由系统强制保留。', '0 disables the logical limit; the system disk reserve remains enforced.') }}</small></label>
        <div class="storage-boundary-note storage-wide">{{ locale.text('通过 STORAGE_PATH 或 Compose 卷挂载配置。目录必须可写并通过启动检查；网页不能把服务指向任意系统路径。', 'Configure this with STORAGE_PATH or a Compose volume. The directory must be writable and pass startup checks; the web UI cannot redirect the service to arbitrary system paths.') }}</div>
        <p class="admin-form-error storage-wide" role="alert">{{ errorMessage }}</p>
        <div v-if="localLimitChanged" class="admin-save-row storage-wide"><button class="btn" type="button" :disabled="busy" @click="stageLocal">{{ locale.text('保存为待启用配置', 'Save as pending') }}</button></div>
      </div>

      <form v-else class="storage-form" @submit.prevent="stageS3">
        <label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}<input v-model="endpoint" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com" autocomplete="off"></label>
        <label>Bucket<input v-model="bucket" class="input" maxlength="63" autocomplete="off"></label>
        <label>Region<input v-model="region" class="input" maxlength="64" autocomplete="off"></label>
        <label class="storage-wide">{{ locale.text('Prefix（可留空，以 / 结尾）', 'Prefix (optional, ending in /)') }}<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/" autocomplete="off"></label>
        <label>{{ locale.text('Access Key ID（保存时必须重新输入）', 'Access Key ID (re-enter to save)') }}<input v-model="accessKeyId" class="input" maxlength="256" autocomplete="off"></label>
        <label>{{ locale.text('Secret Access Key（保存时必须重新输入）', 'Secret Access Key (re-enter to save)') }}<input v-model="secretAccessKey" class="input" type="password" maxlength="4096" autocomplete="new-password"></label>
        <label>{{ locale.text('寻址方式', 'Addressing style') }}<select v-model="addressingStyle" class="input" :disabled="isOfficialCloud"><option value="path">Path Style</option><option value="virtual_hosted">Virtual Hosted</option></select></label>
        <label>{{ locale.text('Ycloud 容量上限（GiB）', 'Ycloud capacity limit (GiB)') }}<input v-model.number="capacityLimitGiB" class="input" type="number" min="0" max="4194304" step="1"><small>{{ locale.text('0 表示不设置逻辑上限；仅统计当前 Prefix 的用户对象。', '0 disables the logical limit; only user objects under this prefix are counted.') }}</small></label>
        <div class="storage-boundary-note">{{ locale.text('MinIO、RustFS 和通用端点必须存在于 S3_ALLOWED_ENDPOINTS 精确白名单。保存前会在保留区验证列举、写入、读取、复制和删除，不触碰用户文件。', 'MinIO, RustFS, and generic endpoints must be in the exact S3_ALLOWED_ENDPOINTS allowlist. Before saving, Ycloud checks list, write, read, copy, and delete inside its reserved prefix without touching user files.') }}</div>
        <p class="admin-form-error storage-wide" role="alert">{{ errorMessage }}</p>
        <div class="admin-save-row storage-wide storage-form-actions"><button class="btn secondary" type="button" :disabled="busy" @click="testConnection">{{ locale.text('仅测试连接', 'Test only') }}</button><button class="btn" type="submit" :disabled="busy">{{ busy ? locale.text('验证中…', 'Verifying…') : locale.text('验证并保存待启用配置', 'Verify and save as pending') }}</button></div>
      </form>
    </div>
  </section>

  <div v-if="confirmingActivation && pendingBackend" class="overlay" @click.self="confirmingActivation = false">
    <section class="modal-card" role="dialog" aria-modal="true" aria-labelledby="storage-activate-title">
      <h2 id="storage-activate-title">{{ locale.text('确认切换存储', 'Activate storage backend?') }}</h2>
      <p>{{ locale.text('切换会先重新验证并恢复待处理事务，再等待正在进行的写操作结束。Ycloud 不会迁移文件：切换后看到的是目标存储中原有的内容。', 'Ycloud revalidates the backend, recovers pending transactions, and waits for active writes to finish. Files are not migrated; after switching, you see the content already present in the target backend.') }}</p>
      <p class="danger-text">{{ locale.text('请确认目标存储已经准备完成，并且您理解本地与对象存储是两套独立数据。', 'Confirm that the target is ready and that local and object storage contain independent data.') }}</p>
      <p class="admin-form-error" role="alert">{{ errorMessage }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="busy" @click="confirmingActivation = false">{{ locale.text('取消', 'Cancel') }}</button><button class="btn danger" type="button" :disabled="busy" @click="activatePending">{{ busy ? locale.text('切换中…', 'Activating…') : locale.text('确认切换', 'Activate') }}</button></div>
    </section>
  </div>
</template>

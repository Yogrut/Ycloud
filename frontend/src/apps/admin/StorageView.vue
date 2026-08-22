<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { S3AddressingStyle, S3Provider, StorageBackendView, TestS3StorageRequest } from '../../shared/api/admin'
import { testS3Storage } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ backend: StorageBackendView }>()
const emit = defineEmits<{ tested: [message: string] }>()
const locale = useLocale()

const provider = ref<S3Provider>('minio')
const endpoint = ref('')
const bucket = ref('')
const region = ref('us-east-1')
const prefix = ref('')
const addressingStyle = ref<S3AddressingStyle>('path')
const accessKeyId = ref('')
const secretAccessKey = ref('')
const testing = ref(false)
const errorMessage = ref('')

const providers: Array<{ id: S3Provider; zh: string; en: string }> = [
  { id: 'alibaba_oss', zh: '阿里云 OSS', en: 'Alibaba Cloud OSS' },
  { id: 'tencent_cos', zh: '腾讯云 COS', en: 'Tencent Cloud COS' },
  { id: 'minio', zh: 'MinIO / RustFS', en: 'MinIO / RustFS' },
  { id: 's3_compatible', zh: 'S3 通用协议', en: 'Generic S3-compatible' },
]

const isOfficialCloud = computed(() => provider.value === 'alibaba_oss' || provider.value === 'tencent_cos')

watch(provider, (value) => {
  if (value === 'alibaba_oss' || value === 'tencent_cos') addressingStyle.value = 'virtual_hosted'
  else addressingStyle.value = 'path'
})

watch(() => props.backend, (value) => {
  if (value.type !== 's3') return
  provider.value = value.provider
  endpoint.value = value.endpoint
  bucket.value = value.bucket
  region.value = value.region
  prefix.value = value.prefix
  addressingStyle.value = value.addressing_style
}, { immediate: true })

function requestBody(): TestS3StorageRequest {
  return {
    provider: provider.value,
    endpoint: endpoint.value.trim(),
    bucket: bucket.value.trim(),
    region: region.value.trim(),
    prefix: prefix.value.trim(),
    addressing_style: addressingStyle.value,
    access_key_id: accessKeyId.value,
    secret_access_key: secretAccessKey.value,
  }
}

async function testConnection(): Promise<void> {
  if (testing.value) return
  testing.value = true
  errorMessage.value = ''
  try {
    await testS3Storage(requestBody())
    secretAccessKey.value = ''
    emit('tested', locale.text('S3 连接与最小列举权限验证通过', 'S3 connection and minimum list permission verified'))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('S3 连接测试失败', 'S3 connection test failed')
  } finally {
    testing.value = false
  }
}
</script>

<template>
  <section class="admin-pane form-pane storage-pane glass" aria-labelledby="storage-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="storage-title">{{ locale.text('存储设置', 'Storage') }}</h1>
        <p>{{ locale.text('本地存储继续作为当前活动后端；S3 配置先完成安全连接测试，再进入切换阶段。', 'Local storage remains active. Verify S3 connectivity safely before the later activation stage.') }}</p>
      </div>
      <span class="status-pill">{{ props.backend.type === 'local' ? locale.text('本地存储', 'Local storage') : 'S3' }}</span>
    </header>
    <div class="admin-pane-body storage-body">
      <div class="storage-provider-grid" role="radiogroup" :aria-label="locale.text('S3 类型', 'S3 provider')">
        <button
          v-for="item in providers"
          :key="item.id"
          type="button"
          class="storage-provider"
          :class="{ active: provider === item.id }"
          role="radio"
          :aria-checked="provider === item.id"
          @click="provider = item.id"
        >
          <strong>{{ locale.text(item.zh, item.en) }}</strong>
          <small>{{ item.id === 'minio' ? 'RustFS' : 'S3 SigV4' }}</small>
        </button>
      </div>

      <form class="storage-form" @submit.prevent="testConnection">
        <label class="storage-wide">{{ locale.text('Endpoint 完整地址', 'Full endpoint URL') }}
          <input v-model="endpoint" class="input" type="url" maxlength="2048" placeholder="https://s3.example.com" autocomplete="off">
        </label>
        <label>{{ locale.text('Bucket', 'Bucket') }}<input v-model="bucket" class="input" maxlength="63" autocomplete="off"></label>
        <label>{{ locale.text('Region', 'Region') }}<input v-model="region" class="input" maxlength="64" autocomplete="off"></label>
        <label class="storage-wide">{{ locale.text('Prefix（可留空，以 / 结尾）', 'Prefix (optional, ending in /)') }}<input v-model="prefix" class="input" maxlength="1024" placeholder="ycloud/" autocomplete="off"></label>
        <label>{{ locale.text('Access Key ID', 'Access Key ID') }}<input v-model="accessKeyId" class="input" maxlength="256" autocomplete="off"></label>
        <label>{{ locale.text('Secret Access Key', 'Secret Access Key') }}<input v-model="secretAccessKey" class="input" type="password" maxlength="4096" autocomplete="new-password"></label>
        <label>{{ locale.text('寻址方式', 'Addressing style') }}
          <select v-model="addressingStyle" class="input" :disabled="isOfficialCloud">
            <option value="path">Path Style</option>
            <option value="virtual_hosted">Virtual Hosted</option>
          </select>
        </label>
        <div class="storage-boundary-note">
          {{ locale.text('MinIO、RustFS 和通用端点必须先加入部署环境 S3_ALLOWED_ENDPOINTS 精确白名单；测试不会保存密钥或切换当前存储。', 'MinIO, RustFS, and generic endpoints must be present in the exact S3_ALLOWED_ENDPOINTS deployment allowlist. Testing does not save credentials or switch active storage.') }}
        </div>
        <p class="admin-form-error storage-wide" role="alert">{{ errorMessage }}</p>
        <div class="admin-save-row storage-wide"><button class="btn" type="submit" :disabled="testing">{{ testing ? locale.text('测试中…', 'Testing…') : locale.text('测试连接', 'Test connection') }}</button></div>
      </form>
    </div>
  </section>
</template>

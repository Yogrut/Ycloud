import { computed, reactive, toRefs, watch } from 'vue'
import type { S3AddressingStyle, S3Provider, StorageInstanceView, TestS3StorageRequest } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

export type StorageOption = 'local' | S3Provider
export const MAX_STORAGE_CAPACITY_GIB = 4_194_304
const BYTES_PER_GIB = 1024 ** 3
const MIN_CAPACITY_BYTES = 1024 ** 2

interface StorageForm {
  provider: StorageOption
  storageName: string
  localPath: string
  endpoint: string
  bucket: string
  region: string
  prefix: string
  addressingStyle: S3AddressingStyle
  relayUpload: boolean
  accessKeyId: string
  secretAccessKey: string
  // v-model.number retains an empty input as a string.
  capacityLimitGiB: number | string
  enabled: boolean
  allowGuestAccess: boolean
  allowGuestDownload: boolean
}

function emptyForm(): StorageForm {
  return {
    provider: 'local', storageName: '', localPath: '', endpoint: '', bucket: '',
    region: 'us-east-1', prefix: '', addressingStyle: 'path', relayUpload: false,
    accessKeyId: '', secretAccessKey: '', capacityLimitGiB: 0,
    enabled: true, allowGuestAccess: false, allowGuestDownload: false,
  }
}

export function useStorageForm() {
  const locale = useLocale()
  const form = reactive(emptyForm())
  const isS3 = computed(() => form.provider !== 'local')
  const isOfficialCloud = computed(() => form.provider === 'alibaba_oss' || form.provider === 'tencent_cos')

  watch(() => form.allowGuestAccess, value => {
    if (!value) form.allowGuestDownload = false
  }, { flush: 'sync' })

  function reset(): void {
    Object.assign(form, emptyForm())
  }

  function loadInstance(instance: StorageInstanceView): void {
    const backend = instance.backend
    const allowGuestAccess = instance.allow_guest_access ?? false
    const values: StorageForm = {
      ...emptyForm(),
      storageName: instance.name,
      enabled: instance.enabled ?? true,
      allowGuestAccess,
      allowGuestDownload: allowGuestAccess && (instance.allow_guest_download ?? true),
      capacityLimitGiB: backend.capacity_limit_bytes ? backend.capacity_limit_bytes / BYTES_PER_GIB : 0,
    }
    if (backend.type === 'local') {
      values.localPath = backend.path
    } else {
      values.provider = backend.provider
      values.endpoint = backend.endpoint
      values.bucket = backend.bucket
      values.region = backend.region
      values.prefix = backend.prefix
      values.addressingStyle = backend.addressing_style
      values.relayUpload = backend.relay_upload ?? false
    }
    // The administrator view never supplies credentials. Start blank so edits
    // preserve existing keys rather than reusing another form's secrets.
    Object.assign(form, values)
  }

  function selectProvider(value: StorageOption): void {
    form.provider = value
    form.addressingStyle = isOfficialCloud.value ? 'virtual_hosted' : 'path'
  }

  function capacityLimitBytes(): number | null {
    const value = Number(form.capacityLimitGiB)
    if (!Number.isFinite(value) || value < 0 || value > MAX_STORAGE_CAPACITY_GIB) {
      throw new Error(locale.text('容量上限必须在 0 到 4194304 GiB 之间', 'Capacity must be from 0 to 4194304 GiB'))
    }
    if (value === 0) return null
    const bytes = Math.round(value * BYTES_PER_GIB)
    if (bytes < MIN_CAPACITY_BYTES) {
      throw new Error(locale.text('容量上限不能小于 1 MiB', 'Capacity cannot be less than 1 MiB'))
    }
    return bytes
  }

  function s3RequestBody(): TestS3StorageRequest {
    if (form.provider === 'local') throw new Error('local storage does not use S3 credentials')
    return {
      provider: form.provider,
      endpoint: form.endpoint.trim(),
      bucket: form.bucket.trim(),
      region: form.region.trim(),
      prefix: form.prefix.trim(),
      addressing_style: form.addressingStyle,
      access_key_id: form.accessKeyId,
      secret_access_key: form.secretAccessKey,
      capacity_limit_bytes: capacityLimitBytes(),
      relay_upload: form.relayUpload,
    }
  }

  return { ...toRefs(form), isS3, isOfficialCloud, reset, loadInstance, selectProvider, capacityLimitBytes, s3RequestBody }
}

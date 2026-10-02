import { effectScope } from 'vue'
import { afterEach, describe, expect, it } from 'vitest'
import type { StorageInstanceView } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { MAX_STORAGE_CAPACITY_GIB, useStorageForm } from './useStorageForm'

const scopes: ReturnType<typeof effectScope>[] = []
function storageForm() {
  const scope = effectScope()
  scopes.push(scope)
  return scope.run(() => useStorageForm())!
}

afterEach(() => {
  scopes.splice(0).forEach(scope => scope.stop())
  useLocale().set('zh-CN')
})

const localInstance: StorageInstanceView = {
  id: 'local', name: 'Local', enabled: true, ready: true,
  allow_guest_access: false, allow_guest_download: false,
  backend: { type: 'local', path: '/mnt/data', capacity_limit_bytes: 3 * 1024 ** 3 },
  usage_bytes: 0, reserved_bytes: 0,
}
const s3Instance: StorageInstanceView = {
  ...localInstance, id: 'objects', name: 'Objects', enabled: false,
  allow_guest_access: true, allow_guest_download: true,
  backend: {
    type: 's3', provider: 'minio', endpoint: 'https://s3.example.com', bucket: 'bucket',
    region: 'region', prefix: 'tenant/', addressing_style: 'path', relay_upload: true,
    has_access_key_id: true, has_secret_access_key: true, capacity_limit_bytes: null,
  },
}

describe('storage form', () => {
  it('resets all editable fields without replacing their reactive references', () => {
    const form = storageForm()
    const nameRef = form.storageName
    form.loadInstance(s3Instance)
    form.accessKeyId.value = 'old-id'
    form.secretAccessKey.value = 'old-secret'
    form.reset()
    expect(form.storageName).toBe(nameRef)
    expect(form.provider.value).toBe('local')
    expect(form.storageName.value).toBe('')
    expect(form.localPath.value).toBe('')
    expect(form.endpoint.value).toBe('')
    expect(form.bucket.value).toBe('')
    expect(form.region.value).toBe('us-east-1')
    expect(form.prefix.value).toBe('')
    expect(form.addressingStyle.value).toBe('path')
    expect(form.relayUpload.value).toBe(false)
    expect(form.accessKeyId.value).toBe('')
    expect(form.secretAccessKey.value).toBe('')
    expect(form.capacityLimitGiB.value).toBe(0)
    expect(form.enabled.value).toBe(true)
    expect(form.allowGuestAccess.value).toBe(false)
    expect(form.allowGuestDownload.value).toBe(false)
  })

  it('keeps separate editor instances independent', () => {
    const first = storageForm()
    const second = storageForm()
    first.loadInstance(s3Instance)
    first.secretAccessKey.value = 'secret'
    expect(second.provider.value).toBe('local')
    expect(second.secretAccessKey.value).toBe('')
  })

  it('loads S3 metadata but leaves both saved credentials blank', () => {
    const form = storageForm()
    form.accessKeyId.value = 'unrelated-id'
    form.secretAccessKey.value = 'unrelated-secret'
    form.loadInstance(s3Instance)
    expect(form.s3RequestBody()).toEqual({
      provider: 'minio', endpoint: 'https://s3.example.com', bucket: 'bucket', region: 'region',
      prefix: 'tenant/', addressing_style: 'path', relay_upload: true,
      access_key_id: '', secret_access_key: '', capacity_limit_bytes: null,
    })
    expect(form.storageName.value).toBe('Objects')
    expect(form.enabled.value).toBe(false)
    expect(form.allowGuestAccess.value).toBe(true)
    expect(form.allowGuestDownload.value).toBe(true)
  })

  it('loads local metadata without retaining S3 fields or credentials', () => {
    const form = storageForm()
    form.loadInstance(s3Instance)
    form.secretAccessKey.value = 'secret'
    form.loadInstance(localInstance)
    expect(form.provider.value).toBe('local')
    expect(form.localPath.value).toBe('/mnt/data')
    expect(form.capacityLimitGiB.value).toBe(3)
    expect(form.endpoint.value).toBe('')
    expect(form.secretAccessKey.value).toBe('')
    expect(form.isS3.value).toBe(false)
    expect(() => form.s3RequestBody()).toThrow('local storage does not use S3 credentials')
  })

  it('preserves old access defaults without enabling downloads when access is off', () => {
    const form = storageForm()
    form.loadInstance({ ...localInstance, enabled: undefined, allow_guest_access: true, allow_guest_download: undefined })
    expect(form.enabled.value).toBe(true)
    expect(form.allowGuestDownload.value).toBe(true)
    form.loadInstance({ ...localInstance, allow_guest_access: undefined, allow_guest_download: true })
    expect(form.allowGuestAccess.value).toBe(false)
    expect(form.allowGuestDownload.value).toBe(false)
  })

  it('clears guest downloads synchronously when guest access is turned off', () => {
    const form = storageForm()
    form.loadInstance(s3Instance)
    form.allowGuestAccess.value = false
    expect(form.allowGuestDownload.value).toBe(false)
    form.allowGuestAccess.value = true
    expect(form.allowGuestDownload.value).toBe(false)
  })

  it.each([
    ['alibaba_oss', 'virtual_hosted', true], ['tencent_cos', 'virtual_hosted', true],
    ['minio', 'path', false], ['s3_compatible', 'path', false], ['local', 'path', false],
  ] as const)('selects the existing addressing default for %s', (provider, style, official) => {
    const form = storageForm()
    form.selectProvider(provider)
    expect(form.addressingStyle.value).toBe(style)
    expect(form.isOfficialCloud.value).toBe(official)
    expect(form.isS3.value).toBe(provider !== 'local')
  })

  it('preserves explicit addressing and defaults absent relay settings to direct uploads', () => {
    const form = storageForm()
    if (s3Instance.backend.type !== 's3') throw new Error('Invalid S3 fixture')
    form.loadInstance({ ...s3Instance, backend: { ...s3Instance.backend, addressing_style: 'virtual_hosted', relay_upload: undefined } })
    expect(form.addressingStyle.value).toBe('virtual_hosted')
    expect(form.relayUpload.value).toBe(false)
  })

  it('trims connection fields but preserves credentials exactly and returns a snapshot', () => {
    const form = storageForm()
    form.selectProvider('s3_compatible')
    form.endpoint.value = ' https://s3.example.com '
    form.bucket.value = ' bucket '
    form.region.value = ' region '
    form.prefix.value = ' tenant/ '
    form.accessKeyId.value = ' id '
    form.secretAccessKey.value = ' secret '
    const request = form.s3RequestBody()
    expect(request).toMatchObject({ endpoint: 'https://s3.example.com', bucket: 'bucket', region: 'region', prefix: 'tenant/', access_key_id: ' id ', secret_access_key: ' secret ' })
    form.secretAccessKey.value = 'changed'
    expect(request.secret_access_key).toBe(' secret ')
  })

  it.each([
    { value: 0, expected: null }, { value: '', expected: null },
    { value: 1 / 1024, expected: 1024 ** 2 },
    { value: 0.001, expected: Math.round(0.001 * 1024 ** 3) },
    { value: MAX_STORAGE_CAPACITY_GIB, expected: 2 ** 52 },
  ])('converts capacity $value using the existing rounding and unlimited rules', ({ value, expected }) => {
    const form = storageForm()
    form.capacityLimitGiB.value = value
    expect(form.capacityLimitBytes()).toBe(expected)
  })

  it.each([-1, MAX_STORAGE_CAPACITY_GIB + 1, Number.NaN, Number.POSITIVE_INFINITY, 'invalid'])('rejects out-of-range capacity %s', value => {
    const form = storageForm()
    form.capacityLimitGiB.value = value
    expect(() => form.capacityLimitBytes()).toThrow('容量上限必须在 0 到 4194304 GiB 之间')
  })

  it('rejects nonzero capacities below one MiB before constructing a request', () => {
    const form = storageForm()
    form.selectProvider('minio')
    form.capacityLimitGiB.value = 0.0001
    expect(() => form.s3RequestBody()).toThrow('容量上限不能小于 1 MiB')
  })

  it('uses the existing English validation messages when selected', () => {
    useLocale().set('en')
    const form = storageForm()
    form.capacityLimitGiB.value = -1
    expect(() => form.capacityLimitBytes()).toThrow('Capacity must be from 0 to 4194304 GiB')
    form.capacityLimitGiB.value = 0.0001
    expect(() => form.capacityLimitBytes()).toThrow('Capacity cannot be less than 1 MiB')
  })
})

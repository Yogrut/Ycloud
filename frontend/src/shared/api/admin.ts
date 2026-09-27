import { useLocale } from '../i18n'

export interface TrafficQuota { enabled: boolean; upload: number; download: number }
export interface TrafficUsage { upload: number; download: number }
export interface TrafficCycle { unit: 'hours' | 'days' | 'months'; every: number; anchor: number; offset_minutes: number }
export interface TrafficSettings {
  total: TrafficQuota; guest: TrafficQuota; users_total: TrafficQuota; users: Record<string, TrafficQuota>; cycle: TrafficCycle
}
export interface TrafficInfo {
  settings: TrafficSettings; total: TrafficUsage; guest: TrafficUsage; users_total: TrafficUsage
  users: Record<string, TrafficUsage>; next_reset: number; days: Record<string, TrafficUsage>
}
export function getTraffic(start?: string, end?: string): Promise<TrafficInfo> {
  const query = new URLSearchParams()
  if (start) query.set('start', start)
  if (end) query.set('end', end)
  return adminRequest('/api/admin/traffic?' + query.toString())
}
export function saveTraffic(settings: Partial<Pick<TrafficSettings, 'total' | 'guest' | 'users_total' | 'cycle'>>): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/traffic', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(settings) })
}

import { ApiError, errorMetadata, requestJson, type OperationOutcome } from './client'

const locale = useLocale()

export interface AdminInfo {
  domain_binding?: DomainBindingView
  username: string
  has_global_web_password: boolean
  admin_totp_enabled?: boolean
  admin_recovery_codes_remaining?: number
  shares: WebDavMountView[]
  folder_locks: FolderLockView[]
  max_upload_bytes: number
  max_upload_batch_bytes?: number
  max_upload_batch_entries?: number
  max_archive_bytes: number
  max_archive_entries: number
  deployment_max_upload_bytes?: number
  deployment_max_upload_batch_bytes?: number
  deployment_max_upload_batch_entries?: number
  deployment_max_archive_bytes?: number
  deployment_max_archive_entries?: number
  upload_rate_bytes_per_sec: number
  download_rate_bytes_per_sec: number
  admin_login_failures: number
  web_login_failures: number
  admin_login_block_seconds: number
  web_login_block_seconds: number
  security_log_retention_days: number
  security_log_max_entries: number
  storage_instances: StorageInstanceView[]
  pending_storage_instance: StorageInstanceView | null
  local_storage_path: string
  local_mounts?: LocalMountView[]
  user_accounts?: UserAccountView[]
}

export interface DomainBinding {
  public_url: string
}

export interface DomainBindingView {
  binding: DomainBinding | null
  source: 'none' | 'settings'
}

export function getDomainBinding(): Promise<DomainBindingView> {
  return adminRequest('/api/admin/domain-binding')
}

export function saveDomainBinding(binding: DomainBinding): Promise<DomainBindingView> {
  return adminRequest('/api/admin/domain-binding', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(binding) })
}

export function removeDomainBinding(): Promise<DomainBindingView> {
  return adminRequest('/api/admin/domain-binding', { method: 'DELETE' })
}

export interface LocalMountView {
  mount_id: string
  name: string
  path: string
  storage_id: string | null
  ready: boolean
  total_bytes: number | null
  available_bytes: number | null
}

export interface StoragePermission {
  storage_id: string
  browse: boolean
  download: boolean
  upload: boolean
  create_directory: boolean
  rename: boolean
  move_items: boolean
  copy: boolean
  delete: boolean
}

export interface UserAccountView {
  revision?: string
  id: string
  username: string
  enabled: boolean
  permissions: StoragePermission[]
}

export interface CreateUserAccountRequest {
  traffic?: TrafficQuota
  username: string
  password: string
  enabled: boolean
  permissions: StoragePermission[]
}

export interface UpdateUserAccountRequest {
  expected_revision?: string
  traffic?: TrafficQuota
  username?: string
  password?: string
  enabled?: boolean
  permissions?: StoragePermission[]
}

export interface StorageInstanceView {
  revision?: string
  health_ok?: boolean
  health_checked_at?: number | null
  id: string
  name: string
  enabled?: boolean
  allow_guest_access?: boolean
  allow_guest_download?: boolean
  status?: 'enabled' | 'disabled' | 'abnormal' | 'pending'
  ready: boolean
  backend: StorageBackendView
  usage_bytes: number
  reserved_bytes: number
  capacity_accurate?: boolean
  capacity_reconciling?: boolean
  cleanup_pending_bytes?: number | null
  cleanup_debt_complete?: boolean | null
  staging_cleanup_pending_uploads?: number | null
  staging_cleanup_pending_copies?: number | null
  staging_cleanup_failed_attempts?: number | null
  s3_orphan_uploads?: number | null
  s3_orphan_backups?: number | null
  s3_recovery_pending_records?: number | null
  s3_recovery_oldest_pending_seconds?: number | null
  s3_recovery_consecutive_failures?: number | null
  s3_recovery_last_failure?: string | null
  s3_recovery_last_failure_unix?: number | null
  s3_recovery_next_retry_unix?: number | null
  s3_recovery_running?: boolean | null
}

export type S3Provider = 'alibaba_oss' | 'tencent_cos' | 'minio' | 's3_compatible'
export type S3AddressingStyle = 'path' | 'virtual_hosted'

export type StorageBackendView =
  | { type: 'local'; mount_id?: string; path: string; capacity_limit_bytes: number | null }
  | {
    type: 's3'
    provider: S3Provider
    endpoint: string
    bucket: string
    region: string
    prefix: string
    addressing_style: S3AddressingStyle
    has_access_key_id: boolean
    has_secret_access_key: boolean
    relay_upload?: boolean
    capacity_limit_bytes: number | null
  }

export interface TestS3StorageRequest {
  relay_upload?: boolean
  provider: S3Provider
  endpoint: string
  bucket: string
  region: string
  prefix: string
  addressing_style: S3AddressingStyle
  access_key_id: string
  secret_access_key: string
  capacity_limit_bytes: number | null
}

export interface S3CapabilityReport {
  profile_version: number
  provider: S3Provider
  conditional_create: 's3_if_none_match' | 'oss_forbid_overwrite'
  conditional_update: 's3_if_match' | 'head_then_write_exclusive_prefix'
  conditional_delete: 's3_if_match' | 'head_then_delete_exclusive_prefix'
  activation_verified: string[]
  deferred_until_first_use: string[]
  requires_exclusive_internal_prefix: boolean
}

export interface WebDavMountView {
  revision?: string
  id: string
  storage_id: string
  name: string
  path: string
  username: string | null
  webdav_enabled: boolean
  has_password: boolean
  readonly: boolean
}

export interface CreateWebDavMountRequest {
  storage_id?: string
  name: string
  path: string
  username?: string
  webdav_enabled: boolean
  password?: string
  readonly: boolean
}

export interface UpdateWebDavMountRequest {
  expected_revision?: string
  storage_id?: string
  name?: string
  path?: string
  username?: string
  webdav_enabled?: boolean
  password?: string
  readonly?: boolean
}

export interface FolderLockView {
  revision?: string
  id: string
  storage_id: string
  path: string
}

export interface CreateFolderLockRequest {
  storage_id?: string
  path: string
  password: string
}

export interface UpdateFolderLockRequest {
  expected_revision?: string
  storage_id?: string
  path?: string
  password?: string
}

export type LoginEntry = 'admin' | 'account' | 'web' | 'web_dav'

export interface LoginEvent {
  id: number
  entry: LoginEntry
  success: boolean
  ip: string
  occurred_at: number
  result: string
  failed_attempts: number
  blocked_until: number | null
  user_agent: string | null
  current_blocked_until: number | null
}

export interface LoginEventPage {
  events: LoginEvent[]
  next_cursor: number | null
  total: number
  page: number
}

export interface UpdateAccountRequest {
  username?: string
  password?: string
  global_web_password?: string
}

export interface UpdateAccountResponse {
  success: boolean
  warning?: string | null
}

export interface UpdateTransferLimitsRequest {
  max_upload_bytes: number
  max_upload_batch_bytes: number
  max_upload_batch_entries: number
  max_archive_bytes: number
  max_archive_entries: number
  upload_rate_bytes_per_sec: number
  download_rate_bytes_per_sec: number
}

export interface UpdateLoginSecuritySettingsRequest {
  admin_login_failures?: number
  web_login_failures?: number
  admin_login_block_seconds?: number
  web_login_block_seconds?: number
  security_log_retention_days?: number
  security_log_max_entries?: number
}

export class AdminApiError extends ApiError {
  constructor(message: string, status: number, code?: string, requestId?: string, operation?: OperationOutcome) {
    super(message, status, code, requestId, operation)
    this.name = 'AdminApiError'
  }
}

async function adminRequest<T>(url: string, options: RequestInit = {}): Promise<T> {
  try {
    return await rawAdminRequest<T>(url, options)
  } catch (error) {
    if (!(error instanceof ApiError) || !error.blocksRetry) throw error
    // Never repeat a write after losing its response. Verify observable settings
    // with a read; secrets and creates cannot be inferred safely from a list.
    const recovered = await readBackMutation(url, options).catch(() => undefined)
    if (recovered) return recovered.value as T
    throw error
  }
}

function matchesFields(actual: unknown, desired: unknown): boolean {
  if (desired === null || typeof desired !== 'object') return actual === desired
  if (Array.isArray(desired)) return JSON.stringify(actual) === JSON.stringify(desired)
  if (!actual || typeof actual !== 'object') return false
  return Object.entries(desired).every(([key, value]) => matchesFields((actual as Record<string, unknown>)[key], value))
}

async function readBackMutation(url: string, options: RequestInit): Promise<{ value: unknown } | undefined> {
  const method = options.method?.toUpperCase()
  if (method === 'PUT' && ['/api/admin/limits', '/api/admin/security/settings', '/api/admin/traffic'].includes(url)) {
    const desired: unknown = JSON.parse(String(options.body))
    const actual = url.endsWith('/traffic')
      ? (await rawAdminRequest<TrafficInfo>('/api/admin/traffic')).settings
      : await rawAdminRequest<AdminInfo>('/api/admin/info')
    return matchesFields(actual, desired) ? { value: { success: true } } : undefined
  }
  const entity = url.match(/^\/api\/admin\/(users|locks|shares|storage)\/([^/]+)$/)
  if (!entity || !['PUT', 'DELETE'].includes(method ?? '')) return undefined
  const info = await rawAdminRequest<AdminInfo>('/api/admin/info')
  const rows = entity[1] === 'users' ? info.user_accounts : entity[1] === 'locks' ? info.folder_locks : entity[1] === 'shares' ? info.shares : info.storage_instances
  const current = rows?.find(row => row.id === decodeURIComponent(entity[2]!))
  if (method === 'DELETE') return !current ? { value: undefined } : undefined
  const desired = JSON.parse(String(options.body)) as Record<string, unknown>
  delete desired.expected_revision
  if ('password' in desired) return undefined
  if ('traffic' in desired && current && entity[1] === 'users') {
    const settings = (await rawAdminRequest<TrafficInfo>('/api/admin/traffic')).settings
    if (!matchesFields(settings.users[current.id], desired.traffic)) return undefined
    delete desired.traffic
  }
  return matchesFields(current, desired) ? { value: current } : undefined
}

async function rawAdminRequest<T>(url: string, options: RequestInit = {}): Promise<T> {
  const { response, body } = await requestJson<T>(url, options)
  if (response.status === 204) return undefined as T
  if (!response.ok) {
    const details = errorMetadata(response, body ?? {})
    throw new AdminApiError(
      details.message ?? (response.status === 401
        ? locale.text('请先登录管理员账户', 'Sign in with an administrator account')
        : locale.t('common.requestFailed', { status: response.status })),
      response.status,
      details.code,
      details.requestId,
      details.operation,
    )
  }
  if (body === undefined) throw new AdminApiError(locale.t('common.invalidResponse'), response.status)
  return body
}

export function getAdminInfo(): Promise<AdminInfo> {
  return adminRequest('/api/admin/info')
}

export function loginAdministrator(username: string, password: string, totpCode?: string): Promise<{ success: boolean; message?: string; is_admin: boolean; totp_required?: boolean }> {
  return adminRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password, totp_code: totpCode || undefined }),
  })
}

export function setupAdministratorTotp(currentPassword: string): Promise<{ secret: string; provisioning_uri: string; qr_svg: string }> {
  return adminRequest('/api/admin/account/totp/setup', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ current_password: currentPassword }),
  })
}

export function enableAdministratorTotp(currentPassword: string, secret: string, code: string): Promise<{ success: boolean; recovery_codes: string[] }> {
  return adminRequest('/api/admin/account/totp/enable', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ current_password: currentPassword, secret, code }),
  })
}

export function disableAdministratorTotp(currentPassword: string, code: string): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/account/totp', {
    method: 'DELETE', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ current_password: currentPassword, code }),
  })
}

export function createUserAccount(body: CreateUserAccountRequest): Promise<UserAccountView> {
  return adminRequest('/api/admin/users', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  })
}

export function updateUserAccount(id: string, body: UpdateUserAccountRequest): Promise<UserAccountView> {
  return adminRequest(`/api/admin/users/${encodeURIComponent(id)}`, {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  })
}

export function deleteUserAccount(id: string): Promise<void> {
  return adminRequest(`/api/admin/users/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

export function updateAccount(body: UpdateAccountRequest): Promise<UpdateAccountResponse> {
  return adminRequest('/api/admin/account', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function updateTransferLimits(body: Partial<UpdateTransferLimitsRequest>): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/limits', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function testS3Storage(body: TestS3StorageRequest): Promise<{ success: boolean; capabilities: S3CapabilityReport }> {
  return adminRequest('/api/admin/storage/test', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function testLocalStorage(path: string): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/local/test', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path }),
  })
}

export function stageS3Storage(name: string, body: TestS3StorageRequest, enabled = true, allowGuestAccess = false, allowGuestDownload?: boolean): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/pending', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, enabled, allow_guest_access: allowGuestAccess, allow_guest_download: allowGuestDownload, ...body }),
  })
}

export function updateS3Storage(storageId: string, name: string, body: TestS3StorageRequest, enabled: boolean, allowGuestAccess: boolean, allowGuestDownload?: boolean, expectedRevision?: string): Promise<{ success: boolean }> {
  return adminRequest(`/api/admin/storage/s3/${encodeURIComponent(storageId)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, enabled, allow_guest_access: allowGuestAccess, allow_guest_download: allowGuestDownload, ...body, expected_revision: expectedRevision }),
  })
}

export function addLocalStorage(path: string, name: string, capacityLimitBytes: number | null, enabled = true, allowGuestAccess = false, allowGuestDownload?: boolean): Promise<{ storage_id: string }> {
  return adminRequest('/api/admin/storage/local', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, name, capacity_limit_bytes: capacityLimitBytes, enabled, allow_guest_access: allowGuestAccess, allow_guest_download: allowGuestDownload }),
  })
}

export function updateLocalStorage(storageId: string, name: string, path: string, capacityLimitBytes: number | null, enabled: boolean, allowGuestAccess: boolean, allowGuestDownload?: boolean, expectedRevision?: string): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/local', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ storage_id: storageId, name, path, capacity_limit_bytes: capacityLimitBytes, enabled, allow_guest_access: allowGuestAccess, allow_guest_download: allowGuestDownload, expected_revision: expectedRevision }),
  })
}

export function deleteStorage(storageId: string): Promise<void> {
  return adminRequest(`/api/admin/storage/${encodeURIComponent(storageId)}`, { method: 'DELETE' })
}

export function activatePendingStorage(): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/activate', { method: 'POST' })
}

export function discardPendingStorage(): Promise<void> {
  return adminRequest('/api/admin/storage/pending', { method: 'DELETE' })
}

export function updateLoginSecuritySettings(body: UpdateLoginSecuritySettingsRequest): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/security/settings', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export interface LoginEventQuery {
  success?: boolean
  since?: number
  entry?: LoginEntry
  ip?: string
  cursor?: number
  limit?: number
  page?: number
  search?: string
}

export function getLoginEvents(query: LoginEventQuery): Promise<LoginEventPage> {
  const params = new URLSearchParams({
    limit: String(query.limit ?? 20),
  })
  if (query.success !== undefined) params.set('success', String(query.success))
  if (query.since !== undefined) params.set('since', String(query.since))
  if (query.page !== undefined) params.set('page', String(query.page))
  if (query.search?.trim()) params.set('search', query.search.trim())
  if (query.entry) params.set('entry', query.entry)
  if (query.ip?.trim()) params.set('ip', query.ip.trim())
  if (query.cursor !== undefined) params.set('cursor', String(query.cursor))
  return adminRequest(`/api/admin/security/events?${params}`)
}

export function clearLoginEvents(): Promise<void> {
  return adminRequest('/api/admin/security/events', { method: 'DELETE' })
}

export function createFolderLock(body: CreateFolderLockRequest): Promise<FolderLockView> {
  return adminRequest('/api/admin/locks', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function updateFolderLock(id: string, body: UpdateFolderLockRequest): Promise<FolderLockView> {
  return adminRequest(`/api/admin/locks/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function deleteFolderLock(id: string): Promise<void> {
  return adminRequest(`/api/admin/locks/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

export function createWebDavMount(body: CreateWebDavMountRequest): Promise<WebDavMountView> {
  return adminRequest('/api/admin/shares', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function updateWebDavMount(id: string, body: UpdateWebDavMountRequest): Promise<WebDavMountView> {
  return adminRequest(`/api/admin/shares/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function deleteWebDavMount(id: string): Promise<void> {
  return adminRequest(`/api/admin/shares/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

export function updateLoginRestriction(action: 'block' | 'unblock', entry: LoginEntry, ip: string): Promise<{ success: boolean }> {
  return adminRequest(`/api/admin/security/${action}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ entry, ip }),
  })
}

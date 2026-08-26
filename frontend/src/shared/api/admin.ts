import { useLocale } from '../i18n'

const locale = useLocale()

export interface AdminInfo {
  username: string
  has_global_web_password: boolean
  shares: WebDavMountView[]
  folder_locks: FolderLockView[]
  max_upload_bytes: number
  max_archive_bytes: number
  max_archive_entries: number
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
  default_storage_id: string
  local_storage_path: string
  local_mounts?: LocalMountView[]
  user_accounts?: UserAccountView[]
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
  id: string
  username: string
  enabled: boolean
  permissions: StoragePermission[]
}

export interface CreateUserAccountRequest {
  username: string
  password: string
  enabled: boolean
  permissions: StoragePermission[]
}

export interface UpdateUserAccountRequest {
  username?: string
  password?: string
  enabled?: boolean
  permissions?: StoragePermission[]
}

export interface StorageInstanceView {
  id: string
  name: string
  is_default: boolean
  enabled?: boolean
  allow_guest_access?: boolean
  status?: 'enabled' | 'disabled' | 'abnormal' | 'pending'
  ready: boolean
  backend: StorageBackendView
  usage_bytes: number
  reserved_bytes: number
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
    capacity_limit_bytes: number | null
  }

export interface TestS3StorageRequest {
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

export interface WebDavMountView {
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
  storage_id?: string
  name?: string
  path?: string
  username?: string
  webdav_enabled?: boolean
  password?: string
  readonly?: boolean
}

export interface FolderLockView {
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

interface ErrorEnvelope {
  message?: string
  error?: { message?: string }
}

export class AdminApiError extends Error {
  constructor(message: string, readonly status: number) {
    super(message)
    this.name = 'AdminApiError'
  }
}

async function readJson<T>(response: Response): Promise<T | undefined> {
  try {
    return await response.json() as T
  } catch {
    return undefined
  }
}

async function adminRequest<T>(url: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(url, { credentials: 'same-origin', ...options })
  if (response.status === 204) return undefined as T
  const body = await readJson<T & ErrorEnvelope>(response)
  if (!response.ok) {
    throw new AdminApiError(
      body?.error?.message ?? body?.message ?? (response.status === 401
        ? locale.text('请先登录管理员账户', 'Sign in with an administrator account')
        : locale.t('common.requestFailed', { status: response.status })),
      response.status,
    )
  }
  if (body === undefined) throw new AdminApiError(locale.t('common.invalidResponse'), response.status)
  return body
}

export function getAdminInfo(): Promise<AdminInfo> {
  return adminRequest('/api/admin/info')
}

export function loginAdministrator(username: string, password: string): Promise<{ success: boolean; message?: string; is_admin: boolean }> {
  return adminRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
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

export function updateTransferLimits(body: UpdateTransferLimitsRequest): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/limits', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function testS3Storage(body: TestS3StorageRequest): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/test', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export function stageS3Storage(name: string, body: TestS3StorageRequest, enabled = true, allowGuestAccess = false): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/pending', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, enabled, allow_guest_access: allowGuestAccess, ...body }),
  })
}

export function updateS3Storage(storageId: string, name: string, body: TestS3StorageRequest): Promise<{ success: boolean }> {
  return adminRequest(`/api/admin/storage/s3/${encodeURIComponent(storageId)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, ...body }),
  })
}

export function addLocalStorage(path: string, name: string, capacityLimitBytes: number | null, enabled = true, allowGuestAccess = false): Promise<{ storage_id: string }> {
  return adminRequest('/api/admin/storage/local', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, name, capacity_limit_bytes: capacityLimitBytes, enabled, allow_guest_access: allowGuestAccess }),
  })
}

export function updateLocalStorage(storageId: string, name: string, path: string, capacityLimitBytes: number | null): Promise<{ success: boolean }> {
  return adminRequest('/api/admin/storage/local', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ storage_id: storageId, name, path, capacity_limit_bytes: capacityLimitBytes }),
  })
}

export function updateStorageAccess(storageId: string, enabled: boolean, allowGuestAccess: boolean): Promise<{ success: boolean }> {
  return adminRequest(`/api/admin/storage/${encodeURIComponent(storageId)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ enabled, allow_guest_access: allowGuestAccess }),
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
  success: boolean
  since: number
  entry?: LoginEntry
  ip?: string
  cursor?: number
  limit?: number
}

export function getLoginEvents(query: LoginEventQuery): Promise<LoginEventPage> {
  const params = new URLSearchParams({
    success: String(query.success),
    since: String(query.since),
    limit: String(query.limit ?? 20),
  })
  if (query.entry) params.set('entry', query.entry)
  if (query.ip?.trim()) params.set('ip', query.ip.trim())
  if (query.cursor !== undefined) params.set('cursor', String(query.cursor))
  return adminRequest(`/api/admin/security/events?${params}`)
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

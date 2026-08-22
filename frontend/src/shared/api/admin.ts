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
}

export interface WebDavMountView {
  id: string
  name: string
  path: string
  username: string | null
  webdav_enabled: boolean
  has_password: boolean
  readonly: boolean
}

export interface CreateWebDavMountRequest {
  name: string
  path: string
  username?: string
  webdav_enabled: boolean
  password?: string
  readonly: boolean
}

export interface UpdateWebDavMountRequest {
  name?: string
  path?: string
  username?: string
  webdav_enabled?: boolean
  password?: string
  readonly?: boolean
}

export interface FolderLockView {
  id: string
  path: string
}

export interface CreateFolderLockRequest {
  path: string
  password: string
}

export interface UpdateFolderLockRequest {
  path?: string
  password?: string
}

export type LoginEntry = 'admin' | 'web' | 'web_dav'

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

export function loginAdministrator(username: string, password: string): Promise<{ success: boolean; message?: string }> {
  return adminRequest('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
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

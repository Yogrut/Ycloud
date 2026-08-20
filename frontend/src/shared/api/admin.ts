export interface AdminInfo {
  username: string
  has_global_web_password: boolean
  shares: unknown[]
  folder_locks: unknown[]
  login_security: unknown[]
  max_upload_bytes: number
  max_archive_bytes: number
  max_archive_entries: number
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
  const body = await readJson<T & ErrorEnvelope>(response)
  if (!response.ok) {
    throw new AdminApiError(
      body?.error?.message ?? body?.message ?? (response.status === 401 ? '请先登录管理员账户' : `请求失败 (${response.status})`),
      response.status,
    )
  }
  if (body === undefined) throw new AdminApiError('服务返回了无效响应', response.status)
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

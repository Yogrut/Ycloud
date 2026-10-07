import { useLocale } from '../i18n'
import { ApiError, errorMetadata, requestJson } from './client'

const locale = useLocale()
const LOGOUT_TIMEOUT_MS = 10_000

export interface Identity {
  logged_in: boolean
  is_admin?: boolean
  username?: string | null
  web_password_required?: boolean
}

export interface UserTraffic {
  usage: { upload: number; download: number }
  quota: { enabled: boolean; upload: number; download: number }
  next_reset: number
}

export async function getUserTraffic(): Promise<UserTraffic> {
  const { response, body } = await requestJson<UserTraffic>('/api/user/traffic', { cache: 'no-store' })
  if (!response.ok || !body?.usage || !body.quota) throw new Error(locale.text('流量信息读取失败', 'Unable to load traffic usage'))
  return body
}

interface GateResponse {
  success?: boolean
  message?: string
  error?: {
    message?: string
  }
}

export async function logoutSession(): Promise<{ success: boolean }> {
  try {
    const { response, body } = await requestJson<GateResponse>('/api/logout', { method: 'POST' }, LOGOUT_TIMEOUT_MS)
    if (!response.ok || body?.success !== true) {
      const details = errorMetadata(response, body)
      throw new ApiError(
        details.message ?? locale.text('退出未完成，请恢复连接后重试。', 'Sign-out was not confirmed. Restore the connection and try again.'),
        response.status, details.code, details.requestId, details.operation,
      )
    }
    return { success: true }
  } catch (error) {
    if (error instanceof ApiError && error.code === 'operation_result_unknown') {
      // Revocation is idempotent, unlike file writes. A later explicit sign-out
      // is allowed, but a lost response must not be reported as confirmed.
      throw new ApiError(locale.text('退出结果未确认，请恢复连接后再次退出。', 'Sign-out was not confirmed. Restore the connection and sign out again.'),
        error.status, error.code, error.requestId, error.operation)
    }
    throw error
  }
}

export async function getIdentity(): Promise<Identity> {
  const { response, body: identity } = await requestJson<Identity>('/api/me')
  if (!response.ok) throw new Error(locale.t('login.identityUnavailable'))

  if (!identity) throw new Error(locale.t('login.invalidIdentity'))
  return identity
}

export async function enterGate(password: string): Promise<void> {
  const { response, body: result } = await requestJson<GateResponse>('/api/gate', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password }),
  })
  if (!response.ok || !result?.success) {
    throw new Error(result?.message ?? result?.error?.message ?? locale.t('login.wrongPassword'))
  }
}

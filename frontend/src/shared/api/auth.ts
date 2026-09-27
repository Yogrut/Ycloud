import { useLocale } from '../i18n'
import { requestJson } from './client'

const locale = useLocale()

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

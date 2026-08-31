import { useLocale } from '../i18n'
import { readJson } from './client'

const locale = useLocale()

export interface Identity {
  logged_in: boolean
  is_admin?: boolean
  username?: string | null
  web_password_required?: boolean
}

interface GateResponse {
  success?: boolean
  message?: string
  error?: {
    message?: string
  }
}

export async function getIdentity(): Promise<Identity> {
  const response = await fetch('/api/me', { credentials: 'same-origin' })
  if (!response.ok) throw new Error(locale.t('login.identityUnavailable'))

  const identity = await readJson<Identity>(response)
  if (!identity) throw new Error(locale.t('login.invalidIdentity'))
  return identity
}

export async function enterGate(password: string): Promise<void> {
  const response = await fetch('/api/gate', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password }),
  })
  const result = await readJson<GateResponse>(response)
  if (!response.ok || !result?.success) {
    throw new Error(result?.message ?? result?.error?.message ?? locale.t('login.wrongPassword'))
  }
}

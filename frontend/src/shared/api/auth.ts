export interface Identity {
  logged_in: boolean
  web_password_required?: boolean
}

interface GateResponse {
  success?: boolean
  message?: string
  error?: {
    message?: string
  }
}

async function readJson<T>(response: Response): Promise<T | undefined> {
  try {
    return await response.json() as T
  } catch {
    return undefined
  }
}

export async function getIdentity(): Promise<Identity> {
  const response = await fetch('/api/me', { credentials: 'same-origin' })
  if (!response.ok) throw new Error('暂时无法检查登录状态')

  const identity = await readJson<Identity>(response)
  if (!identity) throw new Error('服务返回了无效的登录状态')
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
    throw new Error(result?.message ?? result?.error?.message ?? '访问密码错误')
  }
}

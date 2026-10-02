import type { AdminInfo, UpdateAccountRequest } from '../../shared/api/admin'
import { STORED_PASSWORD_MASK } from '../../shared/passwordInput'

type AdminAccountInfo = Pick<AdminInfo, 'username' | 'has_global_web_password'>

export const ADMIN_ACCOUNT_PASSWORD_MINIMUMS = { administrator: 12, browser: 8 } as const

export interface AdminAccountDraft {
  username: string
  adminPassword: string
  webPassword: string
}

export function adminAccountDraft(info: AdminAccountInfo): AdminAccountDraft {
  return {
    username: info.username,
    adminPassword: STORED_PASSWORD_MASK,
    webPassword: info.has_global_web_password ? STORED_PASSWORD_MASK : '',
  }
}

export function adminAccountChanges(draft: AdminAccountDraft, info: AdminAccountInfo): UpdateAccountRequest {
  const changes: UpdateAccountRequest = {}
  const username = draft.username.trim()
  if (username !== info.username) changes.username = username
  if (draft.adminPassword !== STORED_PASSWORD_MASK) changes.password = draft.adminPassword
  // The mask means keep an existing password, not set a password on an unprotected browser.
  const previousWebPassword = info.has_global_web_password ? STORED_PASSWORD_MASK : ''
  if (draft.webPassword !== previousWebPassword) changes.global_web_password = draft.webPassword
  return changes
}

export function buildAdminAccountRequest(
  draft: AdminAccountDraft,
  info: AdminAccountInfo,
  text: (zh: string, en: string) => string,
): UpdateAccountRequest {
  const changes = adminAccountChanges(draft, info)
  if (!draft.username.trim()) {
    throw new Error(text('管理员用户名不能为空', 'Administrator username is required'))
  }
  if (changes.password !== undefined && [...changes.password].length < ADMIN_ACCOUNT_PASSWORD_MINIMUMS.administrator) {
    throw new Error(text('管理员密码至少需要 12 位', 'Administrator password must be at least 12 characters'))
  }
  // Empty clears the browser password. Only nonempty replacements need a minimum length.
  if (changes.global_web_password && [...changes.global_web_password].length < ADMIN_ACCOUNT_PASSWORD_MINIMUMS.browser) {
    throw new Error(text('网页访问密码至少需要 8 位', 'Browser access password must be at least 8 characters'))
  }
  return changes
}

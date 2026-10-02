import type { AdminInfo, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'

const SECONDS_PER_MINUTE = 60
export const LOGIN_PROTECTION_LIMITS = {
  adminFailures: { min: 3, max: 10 },
  webFailures: { min: 3, max: 20 },
  blockMinutes: { min: 5, max: 1440 },
} as const

export type LoginProtectionInfo = Pick<AdminInfo,
  'admin_login_failures' | 'web_login_failures' | 'admin_login_block_seconds' | 'web_login_block_seconds'>
type LoginProtectionRequest = Pick<UpdateLoginSecuritySettingsRequest, keyof LoginProtectionInfo>

export interface LoginProtectionDraft {
  adminFailures: string | number
  adminBlockMinutes: string | number
  webFailures: string | number
  webBlockMinutes: string | number
}

export function loginProtectionDraft(info: LoginProtectionInfo): LoginProtectionDraft {
  return {
    adminFailures: info.admin_login_failures,
    adminBlockMinutes: info.admin_login_block_seconds / SECONDS_PER_MINUTE,
    webFailures: info.web_login_failures,
    webBlockMinutes: info.web_login_block_seconds / SECONDS_PER_MINUTE,
  }
}

function blockSeconds(value: string | number, originalSeconds: number): number {
  const minutes = Number(value)
  // Server settings have second precision, even though this editor uses minutes.
  // Do not convert an unchanged display value back into seconds.
  return minutes === originalSeconds / SECONDS_PER_MINUTE ? originalSeconds : minutes * SECONDS_PER_MINUTE
}

export function loginProtectionChanges(draft: LoginProtectionDraft, info: LoginProtectionInfo): LoginProtectionRequest {
  const changes: LoginProtectionRequest = {}
  const adminFailures = Number(draft.adminFailures)
  const webFailures = Number(draft.webFailures)
  const adminSeconds = blockSeconds(draft.adminBlockMinutes, info.admin_login_block_seconds)
  const webSeconds = blockSeconds(draft.webBlockMinutes, info.web_login_block_seconds)
  if (adminFailures !== info.admin_login_failures) changes.admin_login_failures = adminFailures
  if (webFailures !== info.web_login_failures) changes.web_login_failures = webFailures
  if (adminSeconds !== info.admin_login_block_seconds) changes.admin_login_block_seconds = adminSeconds
  if (webSeconds !== info.web_login_block_seconds) changes.web_login_block_seconds = webSeconds
  return changes
}

function validInteger(value: number, bounds: { min: number; max: number }): boolean {
  return Number.isInteger(value) && value >= bounds.min && value <= bounds.max
}

export function buildLoginProtectionRequest(
  draft: LoginProtectionDraft,
  info: LoginProtectionInfo,
  text: (zh: string, en: string) => string,
): LoginProtectionRequest {
  const changes = loginProtectionChanges(draft, info)
  if (!validInteger(Number(draft.adminFailures), LOGIN_PROTECTION_LIMITS.adminFailures)) {
    throw new Error(text('管理员错误次数必须在 3 到 10 之间', 'Administrator failures must be between 3 and 10'))
  }
  if (!validInteger(Number(draft.webFailures), LOGIN_PROTECTION_LIMITS.webFailures)) {
    throw new Error(text('首页错误次数必须在 3 到 20 之间', 'Browser failures must be between 3 and 20'))
  }
  // Only edited durations must use whole minutes. Unedited valid server seconds
  // must not prevent an unrelated failure-limit change.
  if ((changes.admin_login_block_seconds !== undefined && !validInteger(Number(draft.adminBlockMinutes), LOGIN_PROTECTION_LIMITS.blockMinutes))
    || (changes.web_login_block_seconds !== undefined && !validInteger(Number(draft.webBlockMinutes), LOGIN_PROTECTION_LIMITS.blockMinutes))) {
    throw new Error(text('封禁时间必须在 5 到 1440 分钟之间', 'Block duration must be between 5 and 1440 minutes'))
  }
  return changes
}

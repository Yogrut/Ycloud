import { describe, expect, it } from 'vitest'
import { buildLoginProtectionRequest, loginProtectionChanges, loginProtectionDraft, type LoginProtectionInfo } from './loginProtectionForm'

const info: LoginProtectionInfo = {
  admin_login_failures: 3, web_login_failures: 5,
  admin_login_block_seconds: 3600, web_login_block_seconds: 3600,
}
const text = (zh: string) => zh

describe('login protection draft and differences', () => {
  it('loads only protection settings into independent drafts', () => {
    const current = { ...info }
    Object.defineProperty(current, 'user_accounts', { get: () => { throw new Error('unrelated data read') } })
    const draft = loginProtectionDraft(current)
    expect(draft).toEqual({ adminFailures: 3, webFailures: 5, adminBlockMinutes: 60, webBlockMinutes: 60 })
    expect(loginProtectionChanges(draft, current)).toEqual({})
    expect(buildLoginProtectionRequest(draft, current, text)).toEqual({})
    draft.adminFailures = 7
    expect(current.admin_login_failures).toBe(3)
    expect(loginProtectionDraft(current).adminFailures).toBe(3)
  })

  it.each([
    ['adminFailures', 4, 'admin_login_failures', 4],
    ['webFailures', 6, 'web_login_failures', 6],
    ['adminBlockMinutes', 15, 'admin_login_block_seconds', 900],
    ['webBlockMinutes', 20, 'web_login_block_seconds', 1200],
  ] as const)('uses the same difference for the changed %s input and the request', (input, value, field, expected) => {
    const draft = loginProtectionDraft(info)
    draft[input] = value
    const request = buildLoginProtectionRequest(draft, info, text)
    expect(request).toEqual({ [field]: expected })
    expect(loginProtectionChanges(draft, info)).toEqual(request)
    draft[input] = 0
    expect(request[field]).toBe(expected)
  })

  it('treats numeric strings and surrounding whitespace as unchanged', () => {
    const draft = { adminFailures: ' 3 ', webFailures: '05', adminBlockMinutes: '60.0', webBlockMinutes: ' 60 ' }
    expect(loginProtectionChanges(draft, info)).toEqual({})
    expect(buildLoginProtectionRequest(draft, info, text)).toEqual({})
  })

  it.each([300, 301, 307, 997, 65537, 86399, 86400])('preserves valid original seconds without minute-rounding or validation: %i', seconds => {
    const current = { ...info, admin_login_block_seconds: seconds, web_login_block_seconds: seconds }
    const draft = loginProtectionDraft(current)
    expect(loginProtectionChanges(draft, current)).toEqual({})
    draft.adminFailures = 4
    expect(buildLoginProtectionRequest(draft, current, text)).toEqual({ admin_login_failures: 4 })
    draft.adminBlockMinutes = 10
    expect(buildLoginProtectionRequest(draft, current, text)).toEqual({ admin_login_failures: 4, admin_login_block_seconds: 600 })
    draft.adminBlockMinutes = seconds / 60
    expect(buildLoginProtectionRequest(draft, current, text)).toEqual({ admin_login_failures: 4 })
  })

  it('does not depend on info field order or include log retention settings', () => {
    const current = { security_log_retention_days: 7, web_login_block_seconds: 3600, admin_login_block_seconds: 3600, web_login_failures: 5, admin_login_failures: 3 }
    const draft = loginProtectionDraft(current)
    draft.webFailures = 7
    expect(buildLoginProtectionRequest(draft, current, text)).toEqual({ web_login_failures: 7 })
  })
})

describe('login protection validation', () => {
  it.each([3, 10])('accepts administrator failure boundaries: %i', value => {
    const draft = loginProtectionDraft(info)
    draft.adminFailures = value
    expect(() => buildLoginProtectionRequest(draft, info, text)).not.toThrow()
  })

  it.each([3, 20])('accepts browser failure boundaries: %i', value => {
    const draft = loginProtectionDraft(info)
    draft.webFailures = value
    expect(() => buildLoginProtectionRequest(draft, info, text)).not.toThrow()
  })

  it.each(['', 0, 2, 3.5, NaN, Infinity, 21])('rejects invalid failure values: %s', value => {
    for (const input of ['adminFailures', 'webFailures'] as const) {
      const draft = loginProtectionDraft(info)
      draft[input] = value
      expect(() => buildLoginProtectionRequest(draft, info, text)).toThrow('错误次数')
    }
  })

  it('retains the distinct administrator and browser failure ceilings', () => {
    const draft = loginProtectionDraft(info)
    draft.adminFailures = 11
    expect(() => buildLoginProtectionRequest(draft, info, text)).toThrow('3 到 10')
    draft.adminFailures = 10
    draft.webFailures = 11
    expect(buildLoginProtectionRequest(draft, info, text)).toEqual({ admin_login_failures: 10, web_login_failures: 11 })
  })

  it.each([5, 1440])('accepts edited duration boundaries: %i minutes', minutes => {
    const draft = loginProtectionDraft(info)
    draft.adminBlockMinutes = minutes
    draft.webBlockMinutes = minutes
    expect(buildLoginProtectionRequest(draft, info, text)).toEqual({ admin_login_block_seconds: minutes * 60, web_login_block_seconds: minutes * 60 })
  })

  it.each(['', 0, 4, 5.5, 1441, NaN, Infinity])('rejects changed invalid minutes: %s', value => {
    for (const input of ['adminBlockMinutes', 'webBlockMinutes'] as const) {
      const draft = loginProtectionDraft(info)
      draft[input] = value
      expect(() => buildLoginProtectionRequest(draft, info, text)).toThrow('5 到 1440')
    }
  })

  it('keeps error localization in the caller without depending on Vue or locale state', () => {
    const draft = loginProtectionDraft(info)
    draft.adminFailures = 0
    expect(() => buildLoginProtectionRequest(draft, info, (_zh, en) => en)).toThrow('Administrator failures must be between 3 and 10')
  })
})

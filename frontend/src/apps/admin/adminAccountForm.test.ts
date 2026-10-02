import { describe, expect, it } from 'vitest'
import { STORED_PASSWORD_MASK } from '../../shared/passwordInput'
import { adminAccountChanges, adminAccountDraft, buildAdminAccountRequest } from './adminAccountForm'

const info = { username: 'administrator', has_global_web_password: true }
const zh = (chinese: string) => chinese
const en = (_chinese: string, english: string) => english

describe('administrator account form', () => {
  it.each([true, false])('creates an independent draft when browser protection is %s', protectedBrowser => {
    const source = Object.freeze({ ...info, has_global_web_password: protectedBrowser })
    const draft = adminAccountDraft(source)
    expect(draft).toEqual({ username: info.username, adminPassword: STORED_PASSWORD_MASK, webPassword: protectedBrowser ? STORED_PASSWORD_MASK : '' })
    expect(buildAdminAccountRequest(draft, source, zh)).toEqual({})
    draft.username = 'another-administrator'
    draft.adminPassword = 'another-password'
    draft.webPassword = 'browser-password'
    expect(adminAccountDraft(source).username).toBe(info.username)
    expect(source).toEqual({ ...info, has_global_web_password: protectedBrowser })
  })

  it('ignores username padding and unedited masks', () => {
    const draft = { ...adminAccountDraft(info), username: '  administrator \t' }
    expect(adminAccountChanges(draft, info)).toEqual({})
    expect(buildAdminAccountRequest(draft, info, zh)).toEqual({})
  })

  it.each([
    { field: 'username', value: '  renamed  ', expected: { username: 'renamed' } },
    { field: 'adminPassword', value: '  new-password  ', expected: { password: '  new-password  ' } },
    { field: 'webPassword', value: '  web-password  ', expected: { global_web_password: '  web-password  ' } },
    { field: 'webPassword', value: '', expected: { global_web_password: '' } },
  ] as const)('builds only the changed $field field and preserves password whitespace', ({ field, value, expected }) => {
    const draft = { ...adminAccountDraft(info), [field]: value }
    expect(adminAccountChanges(draft, info)).toEqual(expected)
    expect(buildAdminAccountRequest(draft, info, zh)).toEqual(expected)
  })

  it('sets a new browser password without changing administrator credentials', () => {
    const source = { ...info, has_global_web_password: false }
    expect(buildAdminAccountRequest({ ...adminAccountDraft(source), webPassword: 'browser-password' }, source, zh))
      .toEqual({ global_web_password: 'browser-password' })
  })

  it.each(['', ' \t\n'])('rejects an empty normalized username (%j)', username => {
    expect(() => buildAdminAccountRequest({ ...adminAccountDraft(info), username }, info, zh)).toThrow('管理员用户名不能为空')
  })

  it.each(['', 'a'.repeat(11), '😀'.repeat(6)])('rejects a short administrator password (%j)', adminPassword => {
    expect(() => buildAdminAccountRequest({ ...adminAccountDraft(info), adminPassword }, info, zh)).toThrow('管理员密码至少需要 12 位')
  })

  it.each(['a'.repeat(12), '😀'.repeat(12)])('accepts the administrator minimum counted as Unicode characters', adminPassword => {
    expect(buildAdminAccountRequest({ ...adminAccountDraft(info), adminPassword }, info, zh)).toEqual({ password: adminPassword })
  })

  it.each(['a'.repeat(7), '😀'.repeat(4)])('rejects a short browser replacement (%j)', webPassword => {
    expect(() => buildAdminAccountRequest({ ...adminAccountDraft(info), webPassword }, info, zh)).toThrow('网页访问密码至少需要 8 位')
  })

  it.each(['a'.repeat(8), '😀'.repeat(8)])('accepts the browser minimum counted as Unicode characters', webPassword => {
    expect(buildAdminAccountRequest({ ...adminAccountDraft(info), webPassword }, info, zh)).toEqual({ global_web_password: webPassword })
  })

  it('does not treat a newly typed mask as an existing browser password', () => {
    const source = { ...info, has_global_web_password: false }
    const draft = { ...adminAccountDraft(source), webPassword: STORED_PASSWORD_MASK }
    expect(adminAccountChanges(draft, source)).toEqual({ global_web_password: STORED_PASSWORD_MASK })
    expect(() => buildAdminAccountRequest(draft, source, zh)).toThrow('网页访问密码至少需要 8 位')
  })

  it.each([
    { draft: { username: '' }, message: 'Administrator username is required' },
    { draft: { adminPassword: 'short' }, message: 'Administrator password must be at least 12 characters' },
    { draft: { webPassword: 'short' }, message: 'Browser access password must be at least 8 characters' },
  ])('uses the supplied translation without reading global language state', ({ draft, message }) => {
    expect(() => buildAdminAccountRequest({ ...adminAccountDraft(info), ...draft }, info, en)).toThrow(message)
  })

  it('keeps generated requests independent of subsequent draft changes', () => {
    const draft = { username: ' renamed ', adminPassword: 'new-password', webPassword: 'new-browser-password' }
    const request = buildAdminAccountRequest(draft, info, zh)
    draft.username = info.username
    draft.adminPassword = STORED_PASSWORD_MASK
    draft.webPassword = STORED_PASSWORD_MASK
    expect(request).toEqual({ username: 'renamed', password: 'new-password', global_web_password: 'new-browser-password' })
    expect(adminAccountChanges(draft, info)).toEqual({})
  })

  it('does not mutate inputs or inspect unrelated administrator settings', () => {
    const source = { ...info, get admin_totp_enabled(): boolean { throw new Error('unrelated setting read') } }
    const draft = Object.freeze({ ...adminAccountDraft(source), username: 'renamed' })
    expect(buildAdminAccountRequest(draft, Object.freeze(source), zh)).toEqual({ username: 'renamed' })
    expect(draft.username).toBe('renamed')
    expect(source.username).toBe(info.username)
  })
})

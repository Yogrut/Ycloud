import { describe, expect, it } from 'vitest'
import type { FolderLockView, WebDavMountView } from '../../shared/api/admin'
import { STORED_PASSWORD_MASK } from '../../shared/passwordInput'
import {
  createFolderLockRequest, createWebDavRequest, displayStoragePath, folderLockChanges,
  normalizeStoragePath, webDavChanges, webDavPassword, type FolderLockDraft, type WebDavDraft,
} from './storageAccessForm'

function mount(): WebDavMountView {
  return { id: 'mount', revision: 'revision-one', storage_id: 'primary', name: 'media', path: 'files', username: 'dav-user', has_password: true, webdav_enabled: true, readonly: false }
}

function mountDraft(): WebDavDraft {
  return { storageId: 'primary', name: ' media ', path: '\\files//', username: ' dav-user ', password: STORED_PASSWORD_MASK, enabled: true, readonly: false }
}

function lock(): FolderLockView {
  return { id: 'lock', revision: 'revision-one', storage_id: 'primary', path: 'files' }
}

function lockDraft(): FolderLockDraft {
  return { storageId: 'primary', path: '/files/', password: STORED_PASSWORD_MASK }
}

describe('storage path input formatting', () => {
  it.each([
    ['', '', '/'], ['///\\', '', '/'], ['\\files//child/', 'files/child', '/files/child'],
    [' /文件 空间/ ', ' /文件 空间/ ', '/ /文件 空间/ '],
    ['files/./../child', 'files/./../child', '/files/./../child'],
    ['files/%2F%20', 'files/%2F%20', '/files/%2F%20'],
  ])('formats separators without applying security normalization: %s', (input, normalized, displayed) => {
    expect(normalizeStoragePath(input)).toBe(normalized)
    expect(displayStoragePath(input)).toBe(displayed)
    expect(normalizeStoragePath(normalized)).toBe(normalized)
  })
})

describe('WebDAV draft requests', () => {
  it('uses the credential flag only to choose the initial placeholder', () => {
    expect(webDavPassword(mount())).toBe(STORED_PASSWORD_MASK)
    expect(webDavPassword({ ...mount(), has_password: false })).toBe('')
  })

  it('creates a snapshot with formatted connection fields and an untrimmed password', () => {
    const draft = mountDraft()
    draft.password = '  replacement password  '
    const body = createWebDavRequest(draft)
    expect(body).toEqual({ storage_id: 'primary', name: 'media', path: 'files', username: 'dav-user', password: draft.password, webdav_enabled: true, readonly: false })
    draft.name = 'later edit'
    draft.password = 'other password'
    expect(body.name).toBe('media')
    expect(body.password).toBe('  replacement password  ')
  })

  it('omits empty optional credentials when creating a disabled mount', () => {
    const body = createWebDavRequest({ ...mountDraft(), username: ' ', password: '', enabled: false })
    expect(body.username).toBeUndefined()
    expect(body.password).toBeUndefined()
    expect(body.webdav_enabled).toBe(false)
  })

  it('returns no changes for equivalent path formatting, trimmed identity and the stored mask', () => {
    expect(webDavChanges(mount(), mountDraft())).toEqual({})
  })

  it.each([
    ['storageId', 'archive', { storage_id: 'archive' }],
    ['name', ' photos ', { name: 'photos' }],
    ['path', '\\archive//files/', { path: 'archive/files' }],
    ['username', ' reader ', { username: 'reader' }],
    ['password', '  replacement password  ', { password: '  replacement password  ' }],
    ['enabled', false, { webdav_enabled: false }],
    ['readonly', true, { readonly: true }],
  ] as const)('includes only the changed %s field', (field, value, expected) => {
    expect(webDavChanges(mount(), { ...mountDraft(), [field]: value })).toEqual(expected)
  })

  it('keeps absent credentials unchanged but allows setting a new password', () => {
    const original = { ...mount(), username: null, has_password: false, webdav_enabled: false }
    const draft = { ...mountDraft(), username: ' ', password: '', enabled: false }
    expect(webDavChanges(original, draft)).toEqual({})
    draft.password = 'new password'
    expect(webDavChanges(original, draft)).toEqual({ password: 'new password' })
  })

  it('preserves explicit credential clearing in updates instead of converting it to omission', () => {
    expect(webDavChanges(mount(), { ...mountDraft(), username: ' ', password: '', enabled: false })).toEqual({ username: '', password: '', webdav_enabled: false })
  })

  it('does not mutate the original view or put its revision in the editable changes', () => {
    const original = Object.freeze(mount())
    const changes = webDavChanges(original, { ...mountDraft(), name: 'other' })
    expect(changes).toEqual({ name: 'other' })
    expect(original.name).toBe('media')
    expect(original.revision).toBe('revision-one')
  })
})

describe('folder lock draft requests', () => {
  it('creates a snapshot without trimming password or filename whitespace', () => {
    const draft = { ...lockDraft(), path: '\\files// child /', password: '  lock password  ' }
    const body = createFolderLockRequest(draft)
    expect(body).toEqual({ storage_id: 'primary', path: 'files/ child ', password: '  lock password  ' })
    draft.path = 'other'
    expect(body.path).toBe('files/ child ')
  })

  it('does not submit equivalent path formatting or the stored password mask', () => {
    expect(folderLockChanges(lock(), lockDraft())).toEqual({})
  })

  it.each([
    ['storageId', 'archive', { storage_id: 'archive' }],
    ['path', '\\other//files/', { path: 'other/files' }],
    ['password', '  replacement password  ', { password: '  replacement password  ' }],
  ] as const)('includes only the changed %s field', (field, value, expected) => {
    expect(folderLockChanges(lock(), { ...lockDraft(), [field]: value })).toEqual(expected)
  })

  it('leaves root and password validation to the lock editor and server', () => {
    expect(folderLockChanges(lock(), { ...lockDraft(), path: '/', password: '' })).toEqual({ path: '', password: '' })
  })

  it('does not mutate the original lock or incorporate the revision into editable fields', () => {
    const original = Object.freeze(lock())
    const changes = folderLockChanges(original, { ...lockDraft(), path: 'other' })
    expect(changes).toEqual({ path: 'other' })
    expect(original.path).toBe('files')
    expect(original.revision).toBe('revision-one')
  })
})

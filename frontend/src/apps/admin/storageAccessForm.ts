import type {
  CreateFolderLockRequest, CreateWebDavMountRequest, FolderLockView,
  UpdateFolderLockRequest, UpdateWebDavMountRequest, WebDavMountView,
} from '../../shared/api/admin'
import { STORED_PASSWORD_MASK } from '../../shared/passwordInput'

export interface FolderLockDraft {
  storageId: string
  path: string
  password: string
}

export interface WebDavDraft extends FolderLockDraft {
  name: string
  username: string
  enabled: boolean
  readonly: boolean
}

// Input formatting only: do not decode URLs, trim filename whitespace or resolve
// dot segments here. Authorization and path safety remain server responsibilities.
export function normalizeStoragePath(value: string): string {
  return value.replace(/\\/g, '/').split('/').filter(Boolean).join('/')
}

export function displayStoragePath(value: string): string {
  const normalized = normalizeStoragePath(value)
  return normalized ? `/${normalized}` : '/'
}

export function webDavPassword(mount: WebDavMountView): string {
  return mount.has_password ? STORED_PASSWORD_MASK : ''
}

export function createWebDavRequest(draft: WebDavDraft): CreateWebDavMountRequest {
  return {
    storage_id: draft.storageId, name: draft.name.trim(), path: normalizeStoragePath(draft.path),
    username: draft.username.trim() || undefined, password: draft.password || undefined,
    webdav_enabled: draft.enabled, readonly: draft.readonly,
  }
}

export function webDavChanges(mount: WebDavMountView, draft: WebDavDraft): Omit<UpdateWebDavMountRequest, 'expected_revision'> {
  const changes: Omit<UpdateWebDavMountRequest, 'expected_revision'> = {}
  const name = draft.name.trim()
  const path = normalizeStoragePath(draft.path)
  const username = draft.username.trim()
  if (draft.storageId !== mount.storage_id) changes.storage_id = draft.storageId
  if (name !== mount.name) changes.name = name
  if (path !== mount.path) changes.path = path
  if (username !== (mount.username ?? '')) changes.username = username
  // Unlike an omitted password, an explicit empty value clears it on update.
  if (draft.password !== webDavPassword(mount)) changes.password = draft.password
  if (draft.enabled !== mount.webdav_enabled) changes.webdav_enabled = draft.enabled
  if (draft.readonly !== mount.readonly) changes.readonly = draft.readonly
  return changes
}

export function createFolderLockRequest(draft: FolderLockDraft): CreateFolderLockRequest {
  return { storage_id: draft.storageId, path: normalizeStoragePath(draft.path), password: draft.password }
}

export function folderLockChanges(lock: FolderLockView, draft: FolderLockDraft): Omit<UpdateFolderLockRequest, 'expected_revision'> {
  const changes: Omit<UpdateFolderLockRequest, 'expected_revision'> = {}
  const path = normalizeStoragePath(draft.path)
  if (draft.storageId !== lock.storage_id) changes.storage_id = draft.storageId
  if (path !== lock.path) changes.path = path
  if (draft.password !== STORED_PASSWORD_MASK) changes.password = draft.password
  return changes
}

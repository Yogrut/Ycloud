import type {
  CreateUserAccountRequest, StoragePermission, TrafficQuota, UpdateUserAccountRequest, UserAccountView,
} from '../../shared/api/admin'
import { sameTrafficQuota } from './trafficQuota'

export type PermissionAction = Exclude<keyof StoragePermission, 'storage_id'>
const PERMISSION_FIELDS: readonly PermissionAction[] = [
  'browse', 'download', 'upload', 'create_directory', 'rename', 'move_items', 'copy', 'delete',
]

export interface UserAccountDraft extends CreateUserAccountRequest {
  traffic: TrafficQuota
}

function emptyPermission(storageId: string): StoragePermission {
  return {
    storage_id: storageId, browse: false, download: false, upload: false,
    create_directory: false, rename: false, move_items: false, copy: false, delete: false,
  }
}

export function permissionDrafts(storageIds: readonly string[], existing: readonly StoragePermission[] = []): StoragePermission[] {
  return storageIds.map(id => {
    const permission = existing.find(item => item.storage_id === id)
    return permission ? { ...permission } : emptyPermission(id)
  })
}

export function changePermission(permissions: readonly StoragePermission[], index: number, action: PermissionAction, value: boolean): StoragePermission[] {
  return permissions.map((permission, current) => {
    if (current !== index) return permission
    if (action === 'browse' && !value) return emptyPermission(permission.storage_id)
    return { ...permission, [action]: value, browse: permission.browse || value }
  })
}

function samePermissions(current: readonly StoragePermission[], previous: readonly StoragePermission[]): boolean {
  // Preserve storage order in requests; compare fields, not object insertion order.
  return current.length === previous.length && current.every((permission, index) => {
    const original = previous[index]!
    return permission.storage_id === original.storage_id
      && PERMISSION_FIELDS.every(field => permission[field] === original[field])
  })
}

export function createUserRequest(draft: UserAccountDraft): CreateUserAccountRequest {
  return {
    username: draft.username.trim(), password: draft.password, enabled: draft.enabled,
    permissions: draft.permissions.filter(permission => permission.browse).map(permission => ({ ...permission })),
    traffic: { ...draft.traffic },
  }
}

export function userAccountChanges(account: UserAccountView, previousQuota: TrafficQuota | undefined, draft: UserAccountDraft): UpdateUserAccountRequest {
  const candidate = createUserRequest(draft)
  const changes: UpdateUserAccountRequest = { expected_revision: account.revision }
  if (candidate.username !== account.username) changes.username = candidate.username
  // The administrator view never reads the existing password. Blank means keep it.
  if (candidate.password) changes.password = candidate.password
  if (candidate.enabled !== account.enabled) changes.enabled = candidate.enabled
  if (!samePermissions(candidate.permissions, account.permissions)) changes.permissions = candidate.permissions
  if (!sameTrafficQuota(draft.traffic, previousQuota)) changes.traffic = candidate.traffic
  return changes
}

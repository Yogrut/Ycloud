import { describe, expect, it } from 'vitest'
import type { StoragePermission, TrafficQuota, UserAccountView } from '../../shared/api/admin'
import { changePermission, createUserRequest, permissionDrafts, userAccountChanges, type UserAccountDraft } from './userAccountForm'

const actions = ['download', 'upload', 'create_directory', 'rename', 'move_items', 'copy', 'delete'] as const

function permission(id = 'primary'): StoragePermission {
  return { ...permissionDrafts([id])[0]!, browse: true, download: true }
}

function account(): UserAccountView {
  return { id: 'reader', revision: 'revision-one', username: 'reader', enabled: true, permissions: [permission()] }
}

function quota(): TrafficQuota {
  return { enabled: true, upload: 100, download: 200 }
}

function draft(): UserAccountDraft {
  return { username: ' reader ', password: '', enabled: true, permissions: [permission()], traffic: quota() }
}

describe('user permission drafts', () => {
  it('follows available storage order, adds independent defaults and drops absent storages', () => {
    const existing = [permission(), permission('removed')]
    const current = permissionDrafts(['archive', 'primary'], existing)
    expect(current.map(item => item.storage_id)).toEqual(['archive', 'primary'])
    expect(current[0]).toEqual({ storage_id: 'archive', browse: false, download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false })
    expect(current[1]).toEqual(existing[0])
    expect(current[1]).not.toBe(existing[0])
    current[1]!.download = false
    current[0]!.upload = true
    expect(existing[0]!.download).toBe(true)
    expect(permissionDrafts(['archive'])[0]!.upload).toBe(false)
  })

  it.each(actions)('enables browse with %s and keeps browse when that action is cleared', action => {
    const original = permissionDrafts(['primary', 'archive'])
    const enabled = changePermission(original, 1, action, true)
    expect(enabled[1]).toMatchObject({ browse: true, [action]: true })
    expect(enabled[0]).toBe(original[0])
    expect(original[1]).toMatchObject({ browse: false, [action]: false })
    const cleared = changePermission(enabled, 1, action, false)
    expect(cleared[1]).toMatchObject({ browse: true, [action]: false })
    expect(enabled[1]![action]).toBe(true)
  })

  it('clears all permissions when browse is revoked; enabling it again does not restore old actions', () => {
    const original = { ...permission(), upload: true, create_directory: true, rename: true, move_items: true, copy: true, delete: true }
    const revoked = changePermission([original], 0, 'browse', false)
    expect(revoked).toEqual(permissionDrafts(['primary']))
    expect(original.upload).toBe(true)
    const restored = changePermission(revoked, 0, 'browse', true)
    expect(restored[0]).toEqual({ ...permissionDrafts(['primary'])[0], browse: true })
  })

  it.each([-1, 1, 99])('ignores an invalid selection index: %s', index => {
    const original = [permission()]
    expect(changePermission(original, index, 'delete', true)).toEqual(original)
    expect(original[0]!.delete).toBe(false)
  })
})

describe('user request changes', () => {
  it('creates an isolated request, trims only username and excludes inaccessible storage', () => {
    const current = draft()
    current.password = '  password stays unchanged  '
    current.permissions.push(...permissionDrafts(['archive']))
    const body = createUserRequest(current)
    expect(body).toEqual({ username: 'reader', password: current.password, enabled: true, permissions: [permission()], traffic: quota() })
    current.permissions[0]!.delete = true
    current.traffic.upload = 999
    expect(body.permissions[0]!.delete).toBe(false)
    expect(body.traffic!.upload).toBe(100)
  })

  it('sends only the original revision for unchanged fields and keeps a blank password', () => {
    expect(userAccountChanges(account(), quota(), draft())).toEqual({ expected_revision: 'revision-one' })
  })

  it('ignores object field order without changing storage array order', () => {
    const original = account()
    const p = original.permissions[0]!
    original.permissions = [{ delete: p.delete, copy: p.copy, move_items: p.move_items, rename: p.rename, create_directory: p.create_directory, upload: p.upload, download: p.download, browse: p.browse, storage_id: p.storage_id }]
    expect(userAccountChanges(original, { download: 200, upload: 100, enabled: true }, draft())).toEqual({ expected_revision: 'revision-one' })
    const current = draft()
    original.permissions.push(permission('archive'))
    current.permissions.unshift(permission('archive'))
    expect(userAccountChanges(original, quota(), current).permissions!.map(item => item.storage_id)).toEqual(['archive', 'primary'])
  })

  it('includes changed identity, enabled state and a replacement password verbatim', () => {
    const current = { ...draft(), username: ' changed ', enabled: false, password: '  replacement password  ' }
    expect(userAccountChanges(account(), quota(), current)).toEqual({ expected_revision: 'revision-one', username: 'changed', enabled: false, password: current.password })
  })

  it.each(actions)('detects a changed %s permission', action => {
    const current = draft()
    current.permissions[0]![action] = !current.permissions[0]![action]
    expect(userAccountChanges(account(), quota(), current)).toEqual({ expected_revision: 'revision-one', permissions: current.permissions })
  })

  it('submits an empty permission list when the final storage grant is revoked', () => {
    const current = draft()
    current.permissions = changePermission(current.permissions, 0, 'browse', false)
    expect(userAccountChanges(account(), quota(), current)).toEqual({ expected_revision: 'revision-one', permissions: [] })
  })

  it.each(['enabled', 'upload', 'download'] as const)('detects a changed traffic %s field', field => {
    const current = draft()
    if (field === 'enabled') current.traffic.enabled = false
    else current.traffic[field]++
    const body = userAccountChanges(account(), quota(), current)
    expect(body).toEqual({ expected_revision: 'revision-one', traffic: current.traffic })
    expect(body.traffic).not.toBe(current.traffic)
  })

  it('sends a default quota when no previous account quota was configured', () => {
    const current = draft()
    current.traffic = { enabled: false, upload: 0, download: 0 }
    expect(userAccountChanges(account(), undefined, current)).toEqual({ expected_revision: 'revision-one', traffic: current.traffic })
  })

  it('does not mutate the source account or let later drafts change the update request', () => {
    const original = account()
    Object.freeze(original.permissions[0])
    Object.freeze(original.permissions)
    Object.freeze(original)
    const current = draft()
    current.permissions[0]!.delete = true
    current.traffic.upload++
    const body = userAccountChanges(original, quota(), current)
    current.permissions[0]!.delete = false
    current.traffic.upload = 999
    expect(body.permissions![0]!.delete).toBe(true)
    expect(body.traffic!.upload).toBe(101)
    expect(original.permissions[0]!.delete).toBe(false)
  })
})

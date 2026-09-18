<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import TrafficQuotaFields from '../../shared/components/TrafficQuotaFields.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import AppSwitch from '../../shared/components/AppSwitch.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import { computed, onMounted, ref, useId } from 'vue'
import type { AdminInfo, StoragePermission, TrafficInfo, TrafficQuota, UserAccountView } from '../../shared/api/admin'
import { createUserAccount, deleteUserAccount, getTraffic, updateUserAccount } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const editing = ref<UserAccountView | null | undefined>()
const username = ref('')
const password = ref('')
const enabled = ref(true)
const permissions = ref<StoragePermission[]>([])
const trafficInfo = ref<TrafficInfo>()
const trafficQuota = ref<TrafficQuota>({ enabled: false, upload: 0, download: 0 })
const trafficReady = ref(false)
const error = ref('')
const saving = ref(false)
const pendingDelete = ref<UserAccountView>()
const deleting = ref(false)
const deleteError = ref('')
const accounts = computed(() => props.info.user_accounts ?? [])
const grantId = useId()
const storageSearch = ref('')
const expandedStorageId = ref('')
const visiblePermissions = computed(() => {
  const query = storageSearch.value.trim().toLocaleLowerCase()
  return permissions.value.map((permission, index) => ({permission, index})).filter(({permission}) =>
    !query || `${storageDisplayName(permission.storage_id)} ${storageDescription(permission.storage_id)}`.toLocaleLowerCase().includes(query))
})
function permissionSummary(permission: StoragePermission): string {
  if (!permission.browse) return locale.text('未授权', 'No access')
  const count = actions.value.filter(([key]) => permission[key]).length
  return count ? locale.text(`已授权 · ${count} 项权限`, `Access · ${count} permissions`) : locale.text('仅浏览', 'Browse only')
}

const actions = computed(() => [
  ['download', locale.text('下载', 'Download')], ['upload', locale.text('上传', 'Upload')],
  ['create_directory', locale.text('新建目录', 'Create folder')],
  ['rename', locale.text('重命名', 'Rename')], ['move_items', locale.text('移动', 'Move')],
  ['copy', locale.text('复制', 'Copy')], ['delete', locale.text('删除', 'Delete')],
] as const)

function emptyPermission(storageId: string): StoragePermission {
  return { storage_id: storageId, browse: false, download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false }
}
function formatTraffic(bytes: number): string {
  if (!bytes) return '∞'
  const power = Math.max(0, Math.min(4, Math.floor(Math.log(bytes) / Math.log(1024))))
  return (bytes / 1024 ** power).toLocaleString(undefined, { maximumFractionDigits: 2 }) + ['B', 'K', 'M', 'G', 'T'][power]
}
function quotaFor(account: UserAccountView): TrafficQuota {
  return trafficInfo.value?.settings.users[account.id] ?? { enabled: false, upload: 0, download: 0 }
}
function quotaSummary(account: UserAccountView): string {
  const quota = quotaFor(account)
  if (!quota.enabled) return '↓∞ | ↑∞'
  return `↓${formatTraffic(quota.download)} | ↑${formatTraffic(quota.upload)}`
}
async function loadTraffic(): Promise<void> {
  trafficInfo.value = await getTraffic()
}
async function openEditor(account?: UserAccountView): Promise<void> {
  editing.value = account ?? null
  username.value = account?.username ?? ''
  password.value = ''
  enabled.value = account?.enabled ?? true
  permissions.value = props.info.storage_instances.map(storage => account?.permissions.find(item => item.storage_id === storage.id) ?? emptyPermission(storage.id))
  error.value = ''
  storageSearch.value = ''
  expandedStorageId.value = ''
  trafficQuota.value = { enabled: false, upload: 0, download: 0 }
  trafficReady.value = !account
  if (!trafficInfo.value) {
    try { await loadTraffic() }
    catch (reason) {
      if (account) error.value = reason instanceof Error ? reason.message : locale.text('流量设置读取失败', 'Unable to load traffic settings')
    }
  }
  if (account && trafficInfo.value) {
    trafficQuota.value = { ...quotaFor(account) }
    trafficReady.value = true
  }
}
function setPermission(index: number, action: keyof StoragePermission, value: boolean): void {
  const next = permissions.value.map(item => ({ ...item }))
  const permission = next[index]
  if (!permission || action === 'storage_id') return
  permission[action] = value
  if (action !== 'browse' && value) permission.browse = true
  if (action === 'browse' && !value) {
    Object.assign(permission, emptyPermission(permission.storage_id))
  }
  permissions.value = next
}
function storageDescription(storageId: string): string {
  const storage = props.info.storage_instances.find(item => item.id === storageId)
  if (!storage) return storageId
  if (storage.backend.type === 'local') return storage.backend.path
  return `S3 · ${storage.backend.bucket}`
}
function storageDisplayName(storageId: string): string {
  const storage = props.info.storage_instances.find(item => item.id === storageId)
  if (!storage) return storageId
  if (storage.id === 'primary' && storage.backend.type === 'local' && storage.name === 'Local storage') {
    return locale.text('本地存储', 'Local storage')
  }
  return storage.name
}
async function save(): Promise<void> {
  if (saving.value) return
  if (!username.value.trim() || (!editing.value && password.value.length < 12)) {
    error.value = locale.text('请输入用户名；新账号密码至少 12 位', 'Enter a username. New account passwords require at least 12 characters.')
    return
  }
  saving.value = true
  error.value = ''
  const activePermissions = permissions.value.filter(item => item.browse)
  try {
    if (!trafficReady.value || (['upload', 'download'] as const).some(direction => !Number.isSafeInteger(trafficQuota.value[direction]) || trafficQuota.value[direction] < 0)) {
      throw new Error(locale.text('请输入有效的上传和下载额度', 'Enter valid upload and download allowances'))
    }
    let saved: UserAccountView
    if (editing.value) {
      saved = await updateUserAccount(editing.value.id, { username: username.value.trim(), password: password.value || undefined, enabled: enabled.value, permissions: activePermissions, traffic: trafficQuota.value })
    } else {
      saved = await createUserAccount({ username: username.value.trim(), password: password.value, enabled: enabled.value, permissions: activePermissions, traffic: trafficQuota.value })
    }
    if (trafficInfo.value) trafficInfo.value.settings.users[saved.id] = { ...trafficQuota.value }
    editing.value = undefined
    emit('changed', locale.text('用户已保存，原会话已撤销', 'User saved and previous sessions were revoked.'))
  } catch (reason) { error.value = reason instanceof Error ? reason.message : locale.t('common.requestFailed', { status: '' }) }
  finally { saving.value = false }
}
onMounted(() => { void loadTraffic().catch(() => undefined) })
async function remove(): Promise<void> {
  if (!pendingDelete.value || deleting.value) return
  const account = pendingDelete.value
  deleting.value = true
  deleteError.value = ''
  try {
    await deleteUserAccount(account.id)
    pendingDelete.value = undefined
    emit('changed', locale.text('用户已删除', 'User deleted.'))
  } catch (reason) { deleteError.value = reason instanceof Error ? reason.message : locale.text('删除失败', 'Delete failed.') }
  finally { deleting.value = false }
}
</script>

<template>
  <section class="admin-pane list-pane glass" aria-labelledby="users-title">
    <header class="admin-pane-head"><div><h1 id="users-title">{{ locale.text('用户管理', 'User management') }}</h1><p>{{ locale.text('用户只能由管理员创建、授权和修改密码，且不能进入管理后台。', 'Only administrators can create users, assign permissions, or change passwords. Users cannot open the admin console.') }}</p></div><button class="btn" type="button" @click="openEditor()">{{ locale.text('新建用户', 'New user') }}</button></header>
    <div class="admin-pane-body user-list">
      <div class="record-table-head user-table-head"><span>{{ locale.text('用户', 'User') }}</span><span>{{ locale.text('状态', 'Status') }}</span><span>{{ locale.text('流量额度', 'Traffic allowance') }}</span><span>{{ locale.text('可访问存储', 'Accessible storage') }}</span><span>{{ locale.text('操作', 'Actions') }}</span></div>
      <div v-if="!accounts.length" class="admin-empty">{{ locale.text('暂无用户', 'No users') }}</div>
      <article v-for="account in accounts" :key="account.id" class="user-row">
        <strong class="admin-record-name" :title="account.username">{{ account.username }}</strong>
        <div><span class="status-pill" :class="{ inactive: !account.enabled }">{{ account.enabled ? locale.text('已启用', 'Enabled') : locale.text('已停用', 'Disabled') }}</span></div>
        <span class="status-pill user-traffic-quota">{{ quotaSummary(account) }}</span>
        <span class="user-storage-count">{{ locale.text(`${account.permissions.length} 个存储`, `${account.permissions.length} storage(s)`) }}</span>
        <div class="row-actions"><button class="record-text-btn" type="button" :title="locale.t('common.edit')" :aria-label="locale.t('common.edit')" @click="openEditor(account)">{{ locale.t('common.edit') }}</button><button class="record-text-btn danger" type="button" :title="locale.t('common.delete')" :aria-label="locale.t('common.delete')" @click="pendingDelete = account; deleteError = ''">{{ locale.t('common.delete') }}</button></div>
      </article>
    </div>
  </section>

  <SettingsDrawer v-if="editing !== undefined" :title="editing ? locale.text('编辑用户', 'Edit user') : locale.text('新建用户', 'New user')" :busy="saving" wide @close="editing = undefined">
    <form class="modal user-editor" @submit.prevent="feedbackRevision++; save()">
      <div class="user-basic-grid"><label>{{ locale.text('用户名', 'Username') }}<input v-model="username" aria-required="true" class="input" maxlength="128" autocomplete="off"></label><label>{{ editing ? locale.text('新密码（留空不改）', 'New password (leave blank to keep)') : locale.text('密码（至少 12 位）', 'Password (12+ characters)') }}<input v-model="password" :aria-required="!editing" class="input" type="password" autocomplete="new-password"></label></div>
      <AppSwitch v-model="enabled" :label="locale.text('启用账号', 'Enable account')" :disabled="saving" />
      <TrafficQuotaFields v-model="trafficQuota" flat :label="locale.text('账号流量额度', 'Account traffic allowance')" :disabled="saving || !trafficReady" />
      <div class="storage-grant-list">
        <input v-model="storageSearch" class="input storage-grant-search" type="search" :placeholder="locale.text('搜索存储名称或路径', 'Search storage name or path')" :aria-label="locale.text('搜索存储', 'Search storage')">
        <p v-if="!visiblePermissions.length" class="field-hint">{{ locale.text('没有匹配的存储', 'No matching storage') }}</p>
        <section v-for="({permission, index}) in visiblePermissions" :key="permission.storage_id" class="storage-grant-row" :class="{ disabled: !permission.browse }">
          <button class="storage-grant-heading" type="button" :aria-expanded="expandedStorageId === permission.storage_id" :aria-controls="`${grantId}-${index}`" @click="expandedStorageId = expandedStorageId === permission.storage_id ? '' : permission.storage_id">
            <span class="storage-grant-identity"><strong>{{ storageDisplayName(permission.storage_id) }}</strong><small :title="storageDescription(permission.storage_id)">{{ storageDescription(permission.storage_id) }}</small></span>
            <span class="storage-grant-summary">{{ permissionSummary(permission) }}</span><span class="storage-grant-chevron" aria-hidden="true" />
          </button>
          <div v-if="expandedStorageId === permission.storage_id" :id="`${grantId}-${index}`" class="storage-grant-details">
            <AppSwitch class="storage-access-option" :model-value="permission.browse" :label="locale.text('允许访问', 'Allow access')" :disabled="saving" @update:model-value="setPermission(index, 'browse', $event)" />
            <label v-for="([action, label]) in actions" :key="action" class="storage-grant-option"><input :checked="permission[action]" :disabled="!permission.browse || saving" type="checkbox" @change="setPermission(index, action, ($event.target as HTMLInputElement).checked)"><span>{{ label }}</span></label>
          </div>
        </section>
      </div>
      <AppFeedback :revision="feedbackRevision" :message="error" />
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="saving" @click="editing = undefined">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="saving">{{ locale.text('确认', 'Confirm') }}</button></div>
    </form>
  </SettingsDrawer>
  <ConfirmDialog v-if="pendingDelete" :title="locale.text('删除用户', 'Delete user')" :message="locale.text('将删除以下用户，是否继续？', 'Delete the following user?')" :target="pendingDelete.username" :detail="locale.text('账号及其访问权限将移除，现有登录会话将失效；存储文件不会被删除。', 'The account and its permissions will be removed and sessions revoked. Stored files will not be deleted.')" :error="deleteError" :busy="deleting" @close="pendingDelete = undefined" @confirm="remove" />
</template>
<style scoped>
.user-editor :deep(.input) { box-sizing: border-box; min-width: 0; height: 36px; min-height: 36px; border-radius: 3px; }
.user-editor .modal-actions .btn { min-height: 36px; height: 36px; padding: 7px 14px; border-radius: 3px; font-size: 14px; font-weight: 400; }
</style>

<script setup lang="ts">
import { computed, ref } from 'vue'
import type { AdminInfo, StoragePermission, UserAccountView } from '../../shared/api/admin'
import { createUserAccount, deleteUserAccount, updateUserAccount } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()
const editing = ref<UserAccountView | null | undefined>()
const username = ref('')
const password = ref('')
const enabled = ref(true)
const permissions = ref<StoragePermission[]>([])
const error = ref('')
const saving = ref(false)
const accounts = computed(() => props.info.user_accounts ?? [])

const actions = computed(() => [
  ['download', locale.text('下载', 'Download')], ['upload', locale.text('上传', 'Upload')],
  ['create_directory', locale.text('新建目录', 'Create folder')],
  ['rename', locale.text('重命名', 'Rename')], ['move_items', locale.text('移动', 'Move')],
  ['copy', locale.text('复制', 'Copy')], ['delete', locale.text('删除', 'Delete')],
] as const)

function emptyPermission(storageId: string): StoragePermission {
  return { storage_id: storageId, browse: false, download: false, upload: false, create_directory: false, rename: false, move_items: false, copy: false, delete: false }
}
function openEditor(account?: UserAccountView): void {
  editing.value = account ?? null
  username.value = account?.username ?? ''
  password.value = ''
  enabled.value = account?.enabled ?? true
  permissions.value = props.info.storage_instances.map(storage => account?.permissions.find(item => item.storage_id === storage.id) ?? emptyPermission(storage.id))
  error.value = ''
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
    if (editing.value) {
      await updateUserAccount(editing.value.id, { username: username.value.trim(), password: password.value || undefined, enabled: enabled.value, permissions: activePermissions })
    } else {
      await createUserAccount({ username: username.value.trim(), password: password.value, enabled: enabled.value, permissions: activePermissions })
    }
    editing.value = undefined
    emit('changed', locale.text('用户已保存，原会话已撤销', 'User saved and previous sessions were revoked.'))
  } catch (reason) { error.value = reason instanceof Error ? reason.message : locale.t('common.requestFailed', { status: '' }) }
  finally { saving.value = false }
}
async function remove(account: UserAccountView): Promise<void> {
  if (!window.confirm(locale.text(`删除用户“${account.username}”？`, `Delete user “${account.username}”?`))) return
  try { await deleteUserAccount(account.id); emit('changed', locale.text('用户已删除', 'User deleted.')) }
  catch (reason) { window.alert(reason instanceof Error ? reason.message : locale.text('删除失败', 'Delete failed.')) }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="users-title">
    <header class="admin-pane-head"><div><h1 id="users-title">{{ locale.text('用户管理', 'User management') }}</h1><p>{{ locale.text('用户只能由管理员创建、授权和修改密码，且不能进入管理后台。', 'Only administrators can create users, assign permissions, or change passwords. Users cannot open the admin console.') }}</p></div><button class="btn" type="button" @click="openEditor()">{{ locale.text('新建用户', 'New user') }}</button></header>
    <div class="admin-pane-body user-list">
      <div v-if="!accounts.length" class="admin-empty">{{ locale.text('暂无用户', 'No users') }}</div>
      <article v-for="account in accounts" :key="account.id" class="user-row">
        <div><strong>{{ account.username }}</strong><p>{{ account.enabled ? locale.text('已启用', 'Enabled') : locale.text('已停用', 'Disabled') }} · {{ locale.text(`可访问 ${account.permissions.length} 个存储`, `${account.permissions.length} storage permission(s)`) }}</p></div>
        <div class="row-actions"><button class="btn secondary" type="button" @click="openEditor(account)">{{ locale.t('common.edit') }}</button><button class="btn danger" type="button" @click="remove(account)">{{ locale.t('common.delete') }}</button></div>
      </article>
    </div>
  </section>

  <div v-if="editing !== undefined" class="overlay" @click.self="editing = undefined">
    <form class="modal user-editor" @submit.prevent="save">
      <h2>{{ editing ? locale.text('编辑用户', 'Edit user') : locale.text('新建用户', 'New user') }}</h2>
      <div class="user-basic-grid"><label>{{ locale.text('用户名', 'Username') }}<input v-model="username" class="input" maxlength="128" autocomplete="off"></label><label>{{ editing ? locale.text('新密码（留空不改）', 'New password (leave blank to keep)') : locale.text('密码（至少 12 位）', 'Password (12+ characters)') }}<input v-model="password" class="input" type="password" autocomplete="new-password"></label></div>
      <label class="toggle-line"><input v-model="enabled" type="checkbox">{{ locale.text('启用账号', 'Enable account') }}</label>
      <div class="storage-grant-list">
        <section v-for="(permission, index) in permissions" :key="permission.storage_id" class="storage-grant-row" :class="{ disabled: !permission.browse }">
          <div class="storage-grant-identity"><strong>{{ storageDisplayName(permission.storage_id) }}</strong><small>{{ storageDescription(permission.storage_id) }}</small></div>
          <label class="storage-access-option"><input :checked="permission.browse" type="checkbox" @change="setPermission(index, 'browse', ($event.target as HTMLInputElement).checked)"><span>{{ locale.text('允许访问', 'Allow access') }}</span></label>
          <label v-for="([action, label]) in actions" :key="action" class="storage-grant-option"><input :checked="permission[action]" :disabled="!permission.browse" type="checkbox" @change="setPermission(index, action, ($event.target as HTMLInputElement).checked)"><span>{{ label }}</span></label>
        </section>
      </div>
      <p class="modal-error">{{ error }}</p>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="editing = undefined">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="saving">{{ saving ? locale.t('common.saving') : locale.t('common.save') }}</button></div>
    </form>
  </div>
</template>

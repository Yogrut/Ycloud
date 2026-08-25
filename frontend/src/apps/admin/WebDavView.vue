<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import type { StorageInstanceView, UpdateWebDavMountRequest, WebDavMountView } from '../../shared/api/admin'
import { createWebDavMount, deleteWebDavMount, updateWebDavMount } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const MASK = '••••••'
const { mounts, storages, defaultStorageId } = defineProps<{
  mounts: WebDavMountView[]
  storages: StorageInstanceView[]
  defaultStorageId: string
}>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()

const editing = ref<WebDavMountView>()
const showEditor = ref(false)
const name = ref('')
const storageId = ref('')
const path = ref('/')
const username = ref('')
const password = ref('')
const webdavEnabled = ref(true)
const readonly = ref(false)
const saving = ref(false)
const deleting = ref(false)
const errorMessage = ref('')
const pendingDelete = ref<WebDavMountView>()

function normalizePath(value: string): string {
  return value.replace(/\\/g, '/').split('/').filter(Boolean).join('/')
}

function displayPath(value: string): string {
  const normalized = normalizePath(value)
  return normalized ? `/${normalized}` : '/'
}

function connectionPath(value: string): string {
  return `/dav/${value.trim()}`
}

function initialPassword(mount: WebDavMountView): string {
  return mount.has_password ? MASK : ''
}

function storageName(id: string): string {
  return storages.find((storage) => storage.id === id)?.name ?? id
}

const hasChanges = computed(() => {
  if (!showEditor.value) return false
  if (!editing.value) return true
  return name.value.trim() !== editing.value.name
    || storageId.value !== editing.value.storage_id
    || normalizePath(path.value) !== editing.value.path
    || username.value.trim() !== (editing.value.username ?? '')
    || password.value !== initialPassword(editing.value)
    || webdavEnabled.value !== editing.value.webdav_enabled
    || readonly.value !== editing.value.readonly
})

function openCreate(): void {
  editing.value = undefined
  name.value = ''
  storageId.value = defaultStorageId || storages[0]?.id || ''
  path.value = '/'
  username.value = ''
  password.value = ''
  webdavEnabled.value = true
  readonly.value = false
  errorMessage.value = ''
  showEditor.value = true
}

function openEdit(mount: WebDavMountView): void {
  editing.value = mount
  name.value = mount.name
  storageId.value = mount.storage_id
  path.value = displayPath(mount.path)
  username.value = mount.username ?? ''
  password.value = initialPassword(mount)
  webdavEnabled.value = mount.webdav_enabled
  readonly.value = mount.readonly
  errorMessage.value = ''
  showEditor.value = true
}

function closeEditor(): void {
  if (saving.value) return
  showEditor.value = false
  editing.value = undefined
  errorMessage.value = ''
}

function selectMask(event: FocusEvent): void {
  const input = event.target as HTMLInputElement
  if (input.value === MASK) nextTick(() => input.select())
}

function validate(): string | undefined {
  const normalizedName = name.value.trim()
  if (!normalizedName) return locale.text('挂载名称不能为空', 'Mount name is required')
  if (!storageId.value) return locale.text('请选择存储', 'Select a storage')
  if (normalizedName.includes('/') || normalizedName.includes('\\') || normalizedName.includes('\0')) return locale.text('挂载名称不能包含斜杠或空字符', 'Mount name cannot contain slashes or null characters')
  const hasUsablePassword = (editing.value?.has_password && password.value === MASK) || Boolean(password.value)
  if (webdavEnabled.value && (!username.value.trim() || !hasUsablePassword)) return locale.text('启用 WebDAV 必须设置用户名和密码', 'An enabled WebDAV mount requires a username and password')
  if (password.value !== MASK && password.value && [...password.value].length < 12) return locale.text('WebDAV 密码至少需要 12 位', 'WebDAV password must be at least 12 characters')
  return undefined
}

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  const validationError = validate()
  if (validationError) {
    errorMessage.value = validationError
    return
  }

  const normalizedName = name.value.trim()
  const normalizedPath = normalizePath(path.value)
  const normalizedUsername = username.value.trim()
  saving.value = true
  errorMessage.value = ''
  try {
    if (editing.value) {
      const body: UpdateWebDavMountRequest = {}
      if (storageId.value !== editing.value.storage_id) body.storage_id = storageId.value
      if (normalizedName !== editing.value.name) body.name = normalizedName
      if (normalizedPath !== editing.value.path) body.path = normalizedPath
      if (normalizedUsername !== (editing.value.username ?? '')) body.username = normalizedUsername
      if (password.value !== initialPassword(editing.value)) body.password = password.value
      if (webdavEnabled.value !== editing.value.webdav_enabled) body.webdav_enabled = webdavEnabled.value
      if (readonly.value !== editing.value.readonly) body.readonly = readonly.value
      await updateWebDavMount(editing.value.id, body)
      showEditor.value = false
      emit('changed', locale.text(`WebDAV 挂载“${normalizedName}”已更新`, `WebDAV mount “${normalizedName}” updated`))
    } else {
      await createWebDavMount({
        storage_id: storageId.value,
        name: normalizedName,
        path: normalizedPath,
        username: normalizedUsername || undefined,
        password: password.value || undefined,
        webdav_enabled: webdavEnabled.value,
        readonly: readonly.value,
      })
      showEditor.value = false
      emit('changed', locale.text(`WebDAV 挂载“${normalizedName}”已创建`, `WebDAV mount “${normalizedName}” created`))
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    saving.value = false
  }
}

async function confirmDelete(): Promise<void> {
  if (!pendingDelete.value || deleting.value) return
  deleting.value = true
  errorMessage.value = ''
  const mount = pendingDelete.value
  try {
    await deleteWebDavMount(mount.id)
    pendingDelete.value = undefined
    emit('changed', locale.text(`WebDAV 挂载“${mount.name}”已删除`, `WebDAV mount “${mount.name}” deleted`))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('删除失败', 'Unable to delete the mount')
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="webdav-title">
    <header class="admin-pane-head webdav-head">
      <div>
        <h1 id="webdav-title">{{ locale.text('WebDAV 挂载', 'WebDAV mounts') }}</h1>
        <p>{{ locale.text('为外部 WebDAV 客户端配置独立路径、凭据和读写权限。', 'Configure independent paths, credentials, and permissions for external WebDAV clients.') }}</p>
      </div>
      <button class="btn" type="button" @click="openCreate">{{ locale.text('新建挂载', 'New mount') }}</button>
    </header>
    <div class="admin-pane-body webdav-body">
      <div v-if="!mounts.length" class="admin-empty">{{ locale.text('暂无 WebDAV 挂载', 'No WebDAV mounts') }}</div>
      <div v-else class="webdav-list">
        <article v-for="mount in mounts" :key="mount.id" class="webdav-record">
          <div class="webdav-record-main">
            <div class="webdav-record-title">
              <strong>{{ mount.name }}</strong>
              <span class="status-pill" :class="{ inactive: !mount.webdav_enabled }">{{ mount.webdav_enabled ? locale.text('已启用', 'Enabled') : locale.text('已停用', 'Disabled') }}</span>
              <span class="status-pill permission">{{ mount.readonly ? locale.text('只读', 'Read only') : locale.text('读写', 'Read/write') }}</span>
              <span v-if="mount.has_password" class="status-pill credential">{{ locale.text('密码已设置', 'Password set') }}</span>
            </div>
            <div class="webdav-record-meta">{{ locale.text('存储路径', 'Storage path') }} {{ displayPath(mount.path) }} · {{ locale.text('连接路径', 'Connection path') }} {{ connectionPath(mount.name) }}</div>
            <div v-if="storages.length > 1" class="webdav-record-meta">{{ locale.text('所属存储', 'Storage') }} {{ storageName(mount.storage_id) }}</div>
            <div class="webdav-record-meta">{{ locale.text('用户', 'User') }} {{ mount.username || locale.text('未设置', 'Not set') }}</div>
          </div>
          <div class="webdav-actions">
            <button class="btn secondary" type="button" @click="openEdit(mount)">{{ locale.t('common.edit') }}</button>
            <button class="btn danger-outline" type="button" @click="pendingDelete = mount; errorMessage = ''">{{ locale.t('common.delete') }}</button>
          </div>
        </article>
      </div>
    </div>
  </section>

  <div v-if="showEditor" class="overlay" @click.self="closeEditor">
    <form class="modal webdav-modal" role="dialog" aria-modal="true" aria-labelledby="webdav-editor-title" @submit.prevent="submit">
      <h2 id="webdav-editor-title">{{ editing ? locale.text('编辑 WebDAV 挂载', 'Edit WebDAV mount') : locale.text('新建 WebDAV 挂载', 'New WebDAV mount') }}</h2>
      <div class="webdav-form-grid">
        <label class="full-field">
          {{ locale.text('所属存储', 'Storage') }}
          <select v-model="storageId" class="input" required>
            <option v-for="storage in storages" :key="storage.id" :value="storage.id" :disabled="!storage.ready">
              {{ storage.name }}{{ storage.ready ? '' : locale.text('（不可用）', ' (unavailable)') }}
            </option>
          </select>
        </label>
        <label class="webdav-name-field">
          {{ locale.text('挂载名称', 'Mount name') }}
          <input v-model="name" class="input" required maxlength="128">
        </label>
        <label class="webdav-path-field full-field">
          {{ locale.text('存储路径', 'Storage path') }}
          <input v-model="path" class="input" maxlength="4096" :placeholder="locale.text('/ 表示存储根目录', '/ means the storage root')">
        </label>
        <label class="webdav-user-field">
          {{ locale.text('WebDAV 用户名', 'WebDAV username') }}
          <input v-model="username" class="input" maxlength="128" autocomplete="username">
        </label>
        <label class="webdav-password-field">
          {{ locale.text('WebDAV 密码', 'WebDAV password') }}
          <input v-model="password" class="input" type="password" maxlength="1024" autocomplete="new-password" @focus="selectMask">
        </label>
        <label class="full-field connection-path-field">
          {{ locale.text('连接路径', 'Connection path') }}
          <input class="input" :value="connectionPath(name || locale.text('挂载名称', 'mount-name'))" readonly tabindex="-1">
        </label>
      </div>
      <p class="field-hint">{{ locale.text('存储路径保存时自动统一。启用 WebDAV 时需要用户名和至少 12 位密码。', 'Storage paths are normalized when saved. An enabled mount requires a username and a password of at least 12 characters.') }}</p>
      <p class="field-hint">{{ locale.text('挂载路径不能与网页文件夹锁的父、当前或子目录重叠；编辑时保留密码掩码表示不修改密码。', 'A mount path cannot overlap a browser folder lock at any level. Leave the password mask unchanged while editing to keep the current password.') }}</p>
      <div class="webdav-options">
        <label><input v-model="webdavEnabled" type="checkbox">{{ locale.text('启用 WebDAV', 'Enable WebDAV') }}</label>
        <label><input v-model="readonly" type="checkbox">{{ locale.text('只读访问', 'Read-only access') }}</label>
      </div>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="closeEditor">{{ locale.t('common.cancel') }}</button>
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? locale.t('common.saving') : (editing ? locale.t('common.save') : locale.t('common.create')) }}</button>
      </div>
    </form>
  </div>

  <div v-if="pendingDelete" class="overlay" @click.self="pendingDelete = undefined; errorMessage = ''">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="delete-webdav-title">
      <h2 id="delete-webdav-title">{{ locale.text('确认删除 WebDAV 挂载', 'Delete WebDAV mount?') }}</h2>
      <p>{{ locale.text(`删除“${pendingDelete.name}”只会移除连接配置，磁盘中的文件不会被删除。`, `Deleting “${pendingDelete.name}” removes only the connection settings. Files on disk will not be deleted.`) }}</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="deleting" @click="pendingDelete = undefined; errorMessage = ''">{{ locale.t('common.cancel') }}</button>
        <button class="btn danger" type="button" :disabled="deleting" @click="confirmDelete">{{ deleting ? locale.text('删除中…', 'Deleting…') : locale.text('确认删除', 'Delete mount') }}</button>
      </div>
    </section>
  </div>
</template>

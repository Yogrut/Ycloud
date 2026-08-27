<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import type { FolderLockView, StorageInstanceView, UpdateFolderLockRequest } from '../../shared/api/admin'
import { createFolderLock, deleteFolderLock, updateFolderLock } from '../../shared/api/admin'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'

const MASK = '••••••'
const { locks, storages, defaultStorageId } = defineProps<{
  locks: FolderLockView[]
  storages: StorageInstanceView[]
  defaultStorageId: string
}>()
const emit = defineEmits<{ changed: [message: string] }>()
const locale = useLocale()

const editing = ref<FolderLockView>()
const showEditor = ref(false)
const path = ref('')
const storageId = ref('')
const password = ref('')
const saving = ref(false)
const errorMessage = ref('')
const pendingDelete = ref<FolderLockView>()
const deleting = ref(false)

function normalizePath(value: string): string {
  return value.replace(/\\/g, '/').split('/').filter(Boolean).join('/')
}

function displayPath(value: string): string {
  const normalized = normalizePath(value)
  return normalized ? `/${normalized}` : '/'
}

function storageName(id: string): string {
  return storages.find((storage) => storage.id === id)?.name ?? id
}

const hasChanges = computed(() => {
  if (!showEditor.value) return false
  if (!editing.value) return Boolean(path.value.trim() || password.value)
  return storageId.value !== editing.value.storage_id
    || normalizePath(path.value) !== editing.value.path
    || password.value !== MASK
})

function openCreate(): void {
  editing.value = undefined
  path.value = ''
  storageId.value = defaultStorageId || storages[0]?.id || ''
  password.value = ''
  errorMessage.value = ''
  showEditor.value = true
}

function openEdit(lock: FolderLockView): void {
  editing.value = lock
  path.value = displayPath(lock.path)
  storageId.value = lock.storage_id
  password.value = MASK
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
  if (!storageId.value) return locale.text('请选择存储', 'Select a storage')
  if (!normalizePath(path.value)) return locale.text('不能给存储根目录加锁，请填写具体文件夹路径', 'The storage root cannot be locked. Enter a specific folder path')
  if (!editing.value && !password.value) return locale.text('文件夹锁必须设置密码', 'A folder lock password is required')
  if (password.value !== MASK && [...password.value].length < 8) return locale.text('文件夹锁密码至少需要 8 位', 'Folder lock password must be at least 8 characters')
  return undefined
}

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  const validationError = validate()
  if (validationError) {
    errorMessage.value = validationError
    return
  }

  saving.value = true
  errorMessage.value = ''
  const normalizedPath = normalizePath(path.value)
  try {
    if (editing.value) {
      const body: UpdateFolderLockRequest = {}
      if (storageId.value !== editing.value.storage_id) body.storage_id = storageId.value
      if (normalizedPath !== editing.value.path) body.path = normalizedPath
      if (password.value !== MASK) body.password = password.value
      await updateFolderLock(editing.value.id, body)
      showEditor.value = false
      emit('changed', locale.text(`文件夹锁 ${displayPath(normalizedPath)} 已更新`, `Folder lock ${displayPath(normalizedPath)} updated`))
    } else {
      await createFolderLock({ storage_id: storageId.value, path: normalizedPath, password: password.value })
      showEditor.value = false
      emit('changed', locale.text(`文件夹锁 ${displayPath(normalizedPath)} 已创建`, `Folder lock ${displayPath(normalizedPath)} created`))
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
  const lock = pendingDelete.value
  try {
    await deleteFolderLock(lock.id)
    pendingDelete.value = undefined
    emit('changed', locale.text(`文件夹锁 ${displayPath(lock.path)} 已删除`, `Folder lock ${displayPath(lock.path)} deleted`))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('删除失败', 'Unable to delete the lock')
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="locks-title">
    <header class="admin-pane-head locks-head">
      <div>
        <h1 id="locks-title">{{ locale.text('网页文件夹锁', 'Browser folder locks') }}</h1>
        <p>{{ locale.text('保护指定目录及其子目录的网页访问，并与 WebDAV 挂载路径保持隔离。', 'Protect browser access to selected folders and their descendants while keeping them isolated from WebDAV mounts.') }}</p>
      </div>
      <button class="btn" type="button" @click="openCreate">{{ locale.text('新建锁', 'New lock') }}</button>
    </header>
    <div class="admin-pane-body locks-body">
      <div v-if="!locks.length" class="admin-empty">{{ locale.text('暂无网页文件夹锁', 'No browser folder locks') }}</div>
      <div v-else class="locks-list">
        <article v-for="lock in locks" :key="lock.id" class="lock-record">
          <strong class="admin-record-name">{{ displayPath(lock.path) }}</strong>
          <div class="admin-record-value">
            <span>{{ locale.text('所属存储', 'Storage') }}</span>
            <strong>{{ storageName(lock.storage_id) }}</strong>
          </div>
          <div class="admin-record-end">
            <div class="admin-record-status"><span class="status-pill">{{ locale.text('网页保护', 'Browser protected') }}</span></div>
            <div class="lock-actions">
              <button class="record-icon-btn" type="button" :title="locale.text('编辑文件夹锁', 'Edit folder lock')" :aria-label="locale.text('编辑文件夹锁', 'Edit folder lock')" @click="openEdit(lock)"><AppIcon name="rename" :size="18" /></button>
              <button class="record-icon-btn danger" type="button" :title="locale.text('删除文件夹锁', 'Delete folder lock')" :aria-label="locale.text('删除文件夹锁', 'Delete folder lock')" @click="pendingDelete = lock; errorMessage = ''"><AppIcon name="delete" :size="18" /></button>
            </div>
          </div>
        </article>
      </div>
    </div>
  </section>

  <div v-if="showEditor" class="overlay" @click.self="closeEditor">
    <form class="modal compact-editor-modal" role="dialog" aria-modal="true" aria-labelledby="lock-editor-title" @submit.prevent="submit">
      <h2 id="lock-editor-title">{{ editing ? locale.text('编辑文件夹锁', 'Edit folder lock') : locale.text('新建文件夹锁', 'New folder lock') }}</h2>
      <label>
        {{ locale.text('所属存储', 'Storage') }}
        <select v-model="storageId" class="input" required>
          <option v-for="storage in storages" :key="storage.id" :value="storage.id" :disabled="!storage.ready">
            {{ storage.name }}{{ storage.ready ? '' : locale.text('（不可用）', ' (unavailable)') }}
          </option>
        </select>
      </label>
      <label>
        {{ locale.text('网页文件夹路径', 'Browser folder path') }}
        <input v-model="path" class="input" required maxlength="4096" :placeholder="locale.text('例如 /test', 'For example: /test')">
      </label>
      <p class="field-hint">{{ locale.text('保存时自动统一为 /目录；不能是根目录，也不能与已启用 WebDAV 的父、当前或子目录重叠。', 'Paths are normalized when saved. The root is not allowed, and the path cannot overlap an enabled WebDAV mount at any level.') }}</p>
      <label>
        {{ locale.text('锁密码', 'Lock password') }}
        <input v-model="password" class="input" type="password" required minlength="8" maxlength="1024" autocomplete="new-password" @focus="selectMask">
      </label>
      <p class="field-hint">{{ locale.text('至少 8 位；编辑时保留掩码表示不修改密码，不需要保护时请删除该锁。', 'At least 8 characters. Leave the mask unchanged while editing to keep the current password; delete the lock to remove protection.') }}</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="closeEditor">{{ locale.t('common.cancel') }}</button>
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? locale.t('common.saving') : (editing ? locale.t('common.save') : locale.t('common.create')) }}</button>
      </div>
    </form>
  </div>

  <div v-if="pendingDelete" class="overlay" @click.self="pendingDelete = undefined; errorMessage = ''">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="delete-lock-title">
      <h2 id="delete-lock-title">{{ locale.text('确认删除文件夹锁', 'Delete folder lock?') }}</h2>
      <p>{{ locale.text(`删除 ${displayPath(pendingDelete.path)} 的锁后，该目录将不再单独要求网页访问密码，磁盘文件不会被删除。`, `Removing the lock from ${displayPath(pendingDelete.path)} stops its separate browser password prompt. Files on disk will not be deleted.`) }}</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="deleting" @click="pendingDelete = undefined; errorMessage = ''">{{ locale.t('common.cancel') }}</button>
        <button class="btn danger" type="button" :disabled="deleting" @click="confirmDelete">{{ deleting ? locale.text('删除中…', 'Deleting…') : locale.text('确认删除', 'Delete lock') }}</button>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import type { FolderLockView, UpdateFolderLockRequest } from '../../shared/api/admin'
import { createFolderLock, deleteFolderLock, updateFolderLock } from '../../shared/api/admin'

const MASK = '••••••'
const { locks } = defineProps<{ locks: FolderLockView[] }>()
const emit = defineEmits<{ changed: [message: string] }>()

const editing = ref<FolderLockView>()
const showEditor = ref(false)
const path = ref('')
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

const hasChanges = computed(() => {
  if (!showEditor.value) return false
  if (!editing.value) return Boolean(path.value.trim() || password.value)
  return normalizePath(path.value) !== editing.value.path || password.value !== MASK
})

function openCreate(): void {
  editing.value = undefined
  path.value = ''
  password.value = ''
  errorMessage.value = ''
  showEditor.value = true
}

function openEdit(lock: FolderLockView): void {
  editing.value = lock
  path.value = displayPath(lock.path)
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
  if (!normalizePath(path.value)) return '不能给存储根目录加锁，请填写具体文件夹路径'
  if (!editing.value && !password.value) return '文件夹锁必须设置密码'
  if (password.value !== MASK && [...password.value].length < 8) return '文件夹锁密码至少需要 8 位'
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
      if (normalizedPath !== editing.value.path) body.path = normalizedPath
      if (password.value !== MASK) body.password = password.value
      await updateFolderLock(editing.value.id, body)
      showEditor.value = false
      emit('changed', `文件夹锁 ${displayPath(normalizedPath)} 已更新`)
    } else {
      await createFolderLock({ path: normalizedPath, password: password.value })
      showEditor.value = false
      emit('changed', `文件夹锁 ${displayPath(normalizedPath)} 已创建`)
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '保存失败'
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
    emit('changed', `文件夹锁 ${displayPath(lock.path)} 已删除`)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '删除失败'
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="locks-title">
    <header class="admin-pane-head locks-head">
      <div>
        <h1 id="locks-title">网页文件夹锁</h1>
        <p>保护指定目录及其子目录的网页访问，并与 WebDAV 挂载路径保持隔离。</p>
      </div>
      <button class="btn" type="button" @click="openCreate">新建锁</button>
    </header>
    <div class="admin-pane-body locks-body">
      <div v-if="!locks.length" class="admin-empty">暂无网页文件夹锁</div>
      <div v-else class="locks-list">
        <article v-for="lock in locks" :key="lock.id" class="lock-record">
          <div class="lock-record-main">
            <strong>{{ displayPath(lock.path) }}</strong>
            <span class="status-pill">网页保护</span>
          </div>
          <div class="lock-actions">
            <button class="btn secondary" type="button" @click="openEdit(lock)">编辑</button>
            <button class="btn danger-outline" type="button" @click="pendingDelete = lock; errorMessage = ''">删除</button>
          </div>
        </article>
      </div>
    </div>
  </section>

  <div v-if="showEditor" class="overlay" @click.self="closeEditor">
    <form class="modal" role="dialog" aria-modal="true" aria-labelledby="lock-editor-title" @submit.prevent="submit">
      <h2 id="lock-editor-title">{{ editing ? '编辑文件夹锁' : '新建文件夹锁' }}</h2>
      <label>
        网页文件夹路径
        <input v-model="path" class="input" required maxlength="4096" placeholder="例如 /test">
      </label>
      <p class="field-hint">保存时自动统一为 /目录；不能是根目录，也不能与已启用 WebDAV 的父、当前或子目录重叠。</p>
      <label>
        锁密码
        <input v-model="password" class="input" type="password" required minlength="8" maxlength="1024" autocomplete="new-password" @focus="selectMask">
      </label>
      <p class="field-hint">至少 8 位；编辑时保留掩码表示不修改密码，不需要保护时请删除该锁。</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="closeEditor">取消</button>
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? '保存中…' : (editing ? '保存' : '创建') }}</button>
      </div>
    </form>
  </div>

  <div v-if="pendingDelete" class="overlay" @click.self="pendingDelete = undefined; errorMessage = ''">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="delete-lock-title">
      <h2 id="delete-lock-title">确认删除文件夹锁</h2>
      <p>删除 {{ displayPath(pendingDelete.path) }} 的锁后，该目录将不再单独要求网页访问密码，磁盘文件不会被删除。</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="deleting" @click="pendingDelete = undefined; errorMessage = ''">取消</button>
        <button class="btn danger" type="button" :disabled="deleting" @click="confirmDelete">{{ deleting ? '删除中…' : '确认删除' }}</button>
      </div>
    </section>
  </div>
</template>

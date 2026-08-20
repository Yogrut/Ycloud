<script setup lang="ts">
import { computed, nextTick, ref } from 'vue'
import type { UpdateWebDavMountRequest, WebDavMountView } from '../../shared/api/admin'
import { createWebDavMount, deleteWebDavMount, updateWebDavMount } from '../../shared/api/admin'

const MASK = '••••••'
const { mounts } = defineProps<{ mounts: WebDavMountView[] }>()
const emit = defineEmits<{ changed: [message: string] }>()

const editing = ref<WebDavMountView>()
const showEditor = ref(false)
const name = ref('')
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

const hasChanges = computed(() => {
  if (!showEditor.value) return false
  if (!editing.value) return true
  return name.value.trim() !== editing.value.name
    || normalizePath(path.value) !== editing.value.path
    || username.value.trim() !== (editing.value.username ?? '')
    || password.value !== initialPassword(editing.value)
    || webdavEnabled.value !== editing.value.webdav_enabled
    || readonly.value !== editing.value.readonly
})

function openCreate(): void {
  editing.value = undefined
  name.value = ''
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
  if (!normalizedName) return '挂载名称不能为空'
  if (normalizedName.includes('/') || normalizedName.includes('\\') || normalizedName.includes('\0')) return '挂载名称不能包含斜杠或空字符'
  const hasUsablePassword = (editing.value?.has_password && password.value === MASK) || Boolean(password.value)
  if (webdavEnabled.value && (!username.value.trim() || !hasUsablePassword)) return '启用 WebDAV 必须设置用户名和密码'
  if (password.value !== MASK && password.value && [...password.value].length < 12) return 'WebDAV 密码至少需要 12 位'
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
      if (normalizedName !== editing.value.name) body.name = normalizedName
      if (normalizedPath !== editing.value.path) body.path = normalizedPath
      if (normalizedUsername !== (editing.value.username ?? '')) body.username = normalizedUsername
      if (password.value !== initialPassword(editing.value)) body.password = password.value
      if (webdavEnabled.value !== editing.value.webdav_enabled) body.webdav_enabled = webdavEnabled.value
      if (readonly.value !== editing.value.readonly) body.readonly = readonly.value
      await updateWebDavMount(editing.value.id, body)
      showEditor.value = false
      emit('changed', `WebDAV 挂载“${normalizedName}”已更新`)
    } else {
      await createWebDavMount({
        name: normalizedName,
        path: normalizedPath,
        username: normalizedUsername || undefined,
        password: password.value || undefined,
        webdav_enabled: webdavEnabled.value,
        readonly: readonly.value,
      })
      showEditor.value = false
      emit('changed', `WebDAV 挂载“${normalizedName}”已创建`)
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
  const mount = pendingDelete.value
  try {
    await deleteWebDavMount(mount.id)
    pendingDelete.value = undefined
    emit('changed', `WebDAV 挂载“${mount.name}”已删除`)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '删除失败'
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="webdav-title">
    <header class="admin-pane-head webdav-head">
      <div>
        <h1 id="webdav-title">WebDAV 挂载</h1>
        <p>为外部 WebDAV 客户端配置独立路径、凭据和读写权限。</p>
      </div>
      <button class="btn" type="button" @click="openCreate">新建挂载</button>
    </header>
    <div class="admin-pane-body webdav-body">
      <div v-if="!mounts.length" class="admin-empty">暂无 WebDAV 挂载</div>
      <div v-else class="webdav-list">
        <article v-for="mount in mounts" :key="mount.id" class="webdav-record">
          <div class="webdav-record-main">
            <div class="webdav-record-title">
              <strong>{{ mount.name }}</strong>
              <span class="status-pill" :class="{ inactive: !mount.webdav_enabled }">{{ mount.webdav_enabled ? '已启用' : '已停用' }}</span>
              <span class="status-pill permission">{{ mount.readonly ? '只读' : '读写' }}</span>
              <span v-if="mount.has_password" class="status-pill credential">密码已设置</span>
            </div>
            <div class="webdav-record-meta">存储路径 {{ displayPath(mount.path) }} · 连接路径 {{ connectionPath(mount.name) }}</div>
            <div class="webdav-record-meta">用户 {{ mount.username || '未设置' }}</div>
          </div>
          <div class="webdav-actions">
            <button class="btn secondary" type="button" @click="openEdit(mount)">编辑</button>
            <button class="btn danger-outline" type="button" @click="pendingDelete = mount; errorMessage = ''">删除</button>
          </div>
        </article>
      </div>
    </div>
  </section>

  <div v-if="showEditor" class="overlay" @click.self="closeEditor">
    <form class="modal webdav-modal" role="dialog" aria-modal="true" aria-labelledby="webdav-editor-title" @submit.prevent="submit">
      <h2 id="webdav-editor-title">{{ editing ? '编辑 WebDAV 挂载' : '新建 WebDAV 挂载' }}</h2>
      <div class="webdav-form-grid">
        <label>
          挂载名称
          <input v-model="name" class="input" required maxlength="128">
        </label>
        <label>
          存储路径
          <input v-model="path" class="input" maxlength="4096" placeholder="/ 表示存储根目录">
        </label>
        <label>
          WebDAV 用户名
          <input v-model="username" class="input" maxlength="128" autocomplete="username">
        </label>
        <label>
          WebDAV 密码
          <input v-model="password" class="input" type="password" maxlength="1024" autocomplete="new-password" @focus="selectMask">
        </label>
      </div>
      <p class="field-hint">连接路径为 {{ connectionPath(name || '挂载名称') }}；存储路径保存时自动统一。启用 WebDAV 时需要用户名和至少 12 位密码。</p>
      <p class="field-hint">挂载路径不能与网页文件夹锁的父、当前或子目录重叠；编辑时保留密码掩码表示不修改密码。</p>
      <div class="webdav-options">
        <label><input v-model="webdavEnabled" type="checkbox">启用 WebDAV</label>
        <label><input v-model="readonly" type="checkbox">只读访问</label>
      </div>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="closeEditor">取消</button>
        <button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? '保存中…' : (editing ? '保存' : '创建') }}</button>
      </div>
    </form>
  </div>

  <div v-if="pendingDelete" class="overlay" @click.self="pendingDelete = undefined; errorMessage = ''">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="delete-webdav-title">
      <h2 id="delete-webdav-title">确认删除 WebDAV 挂载</h2>
      <p>删除“{{ pendingDelete.name }}”只会移除连接配置，磁盘中的文件不会被删除。</p>
      <p class="modal-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="deleting" @click="pendingDelete = undefined; errorMessage = ''">取消</button>
        <button class="btn danger" type="button" :disabled="deleting" @click="confirmDelete">{{ deleting ? '删除中…' : '确认删除' }}</button>
      </div>
    </section>
  </div>
</template>

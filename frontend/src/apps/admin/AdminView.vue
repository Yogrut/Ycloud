<script setup lang="ts">
import { onMounted, ref } from 'vue'
import type { AdminInfo } from '../../shared/api/admin'
import { AdminApiError, getAdminInfo, loginAdministrator } from '../../shared/api/admin'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import AccountView from './AccountView.vue'
import AdminNavIcon from './AdminNavIcon.vue'

defineProps<{ theme: ThemeController }>()

const info = ref<AdminInfo>()
const loading = ref(true)
const requiresLogin = ref(false)
const username = ref('')
const password = ref('')
const loginError = ref('')
const loggingIn = ref(false)
const notice = ref('')

const navigation = [
  { group: '存储与访问', items: [
    { id: 'webdav', label: 'WebDAV' },
    { id: 'locks', label: '文件夹锁' },
    { id: 'limits', label: '传输限制' },
  ] },
  { group: '账户与安全', items: [
    { id: 'account', label: '账户与访问' },
    { id: 'security', label: '登录安全' },
  ] },
] as const

async function load(): Promise<void> {
  loading.value = true
  try {
    info.value = await getAdminInfo()
    requiresLogin.value = false
  } catch (error) {
    if (error instanceof AdminApiError && (error.status === 401 || error.status === 403)) {
      requiresLogin.value = true
      info.value = undefined
    } else {
      loginError.value = error instanceof Error ? error.message : '管理后台加载失败'
    }
  } finally {
    loading.value = false
  }
}

async function submitLogin(): Promise<void> {
  if (!username.value.trim() || !password.value || loggingIn.value) {
    if (!username.value.trim() || !password.value) loginError.value = '请输入用户名和密码'
    return
  }
  loggingIn.value = true
  loginError.value = ''
  try {
    const result = await loginAdministrator(username.value.trim(), password.value)
    if (!result.success) throw new Error(result.message ?? '登录失败')
    password.value = ''
    await load()
  } catch (error) {
    loginError.value = error instanceof Error ? error.message : '登录失败'
  } finally {
    loggingIn.value = false
  }
}

function showNotice(message: string): void {
  notice.value = message
  window.setTimeout(() => { if (notice.value === message) notice.value = '' }, 3000)
  void load()
}

function sessionExpired(): void {
  window.setTimeout(() => window.location.replace('/v2/browse'), 700)
}

onMounted(load)
</script>

<template>
  <header class="topbar admin-topbar">
    <div class="brand"><CloudIcon /><span>Ycloud · 管理</span></div>
    <div class="top-actions">
      <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
      <a class="icon-btn flat" href="/v2/browse" title="返回文件" aria-label="返回文件">
        <svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" /></svg>
      </a>
    </div>
  </header>

  <main v-if="!requiresLogin" class="admin-shell">
    <aside class="admin-nav glass" aria-label="管理设置">
      <div v-for="group in navigation" :key="group.group" class="admin-nav-group">
        <div class="admin-nav-heading">{{ group.group }}</div>
        <button
          v-for="item in group.items"
          :key="item.id"
          class="admin-nav-item"
          :class="{ active: item.id === 'account' }"
          type="button"
          :disabled="item.id !== 'account'"
          :aria-current="item.id === 'account' ? 'page' : undefined"
          :title="item.id === 'account' ? undefined : '后续阶段迁移'"
        >
          <AdminNavIcon :name="item.id" />
          <span>{{ item.label }}</span>
        </button>
      </div>
    </aside>
    <div class="admin-content">
      <div v-if="loading" class="admin-loading glass">正在加载…</div>
      <AccountView v-else-if="info" :info="info" @saved="showNotice" @expired="sessionExpired" />
      <div v-else class="admin-loading glass">{{ loginError || '管理后台暂时无法加载' }}</div>
    </div>
  </main>

  <main v-else class="admin-login-shell">
    <form class="admin-login-panel glass" @submit.prevent="submitLogin">
      <CloudIcon class="login-logo" />
      <h1>管理员登录</h1>
      <label>用户名<input v-model="username" class="input" autocomplete="username" autofocus></label>
      <label>密码<input v-model="password" class="input" type="password" autocomplete="current-password"></label>
      <p class="admin-form-error" role="alert">{{ loginError }}</p>
      <button class="btn" type="submit" :disabled="loggingIn">{{ loggingIn ? '登录中…' : '登录' }}</button>
    </form>
  </main>

  <div class="toast" :class="{ show: notice }" role="status">{{ notice }}</div>
</template>

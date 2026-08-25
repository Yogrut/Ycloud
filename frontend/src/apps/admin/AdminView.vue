<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { AdminInfo } from '../../shared/api/admin'
import { AdminApiError, getAdminInfo, loginAdministrator } from '../../shared/api/admin'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import AccountView from './AccountView.vue'
import AdminNavIcon from './AdminNavIcon.vue'
import LimitsView from './LimitsView.vue'
import LocksView from './LocksView.vue'
import SecurityView from './SecurityView.vue'
import StorageView from './StorageView.vue'
import WebDavView from './WebDavView.vue'
import UsersView from './UsersView.vue'
import { appPath } from '../../shared/routes'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()

const info = ref<AdminInfo>()
const loading = ref(true)
const requiresLogin = ref(false)
const username = ref('')
const password = ref('')
const loginError = ref('')
const loggingIn = ref(false)
const notice = ref('')
const activeSection = window.location.pathname.endsWith('/security')
  ? 'security'
  : window.location.pathname.endsWith('/limits')
    ? 'limits'
    : window.location.pathname.endsWith('/locks')
      ? 'locks'
      : window.location.pathname.endsWith('/webdav')
        ? 'webdav'
        : window.location.pathname.endsWith('/storage')
          ? 'storage'
          : window.location.pathname.endsWith('/users') ? 'users' : 'account'

const navigation = computed(() => [
  { group: locale.text('存储与访问', 'Storage & access'), items: [
    { id: 'storage', label: locale.text('存储设置', 'Storage'), href: appPath('/admin/storage') },
    { id: 'webdav', label: 'WebDAV', href: appPath('/admin/webdav') },
    { id: 'locks', label: locale.text('文件夹锁', 'Folder locks'), href: appPath('/admin/locks') },
    { id: 'limits', label: locale.text('传输限制', 'Transfer limits'), href: appPath('/admin/limits') },
  ] },
  { group: locale.text('账户与安全', 'Account & security'), items: [
    { id: 'account', label: locale.text('账户与访问', 'Account & access'), href: appPath('/admin/account') },
    { id: 'users', label: locale.text('普通账号', 'User accounts'), href: appPath('/admin/users') },
    { id: 'security', label: locale.text('登录安全', 'Sign-in security'), href: appPath('/admin/security') },
  ] },
] as const)

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
      loginError.value = error instanceof Error ? error.message : locale.text('管理后台加载失败', 'Unable to load the admin console')
    }
  } finally {
    loading.value = false
  }
}

async function submitLogin(): Promise<void> {
  if (!username.value.trim() || !password.value || loggingIn.value) {
    if (!username.value.trim() || !password.value) loginError.value = locale.text('请输入用户名和密码', 'Enter your username and password')
    return
  }
  loggingIn.value = true
  loginError.value = ''
  try {
    const result = await loginAdministrator(username.value.trim(), password.value)
    if (!result.success) throw new Error(result.message ?? locale.text('登录失败', 'Sign-in failed'))
    if (!result.is_admin) {
      await fetch('/api/logout', { method: 'POST', credentials: 'same-origin' })
      throw new Error(locale.text('普通账号不能进入管理后台', 'This account cannot open the admin console.'))
    }
    password.value = ''
    await load()
  } catch (error) {
    loginError.value = error instanceof Error ? error.message : locale.text('登录失败', 'Sign-in failed')
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
  window.setTimeout(() => window.location.replace(appPath('/browse')), 700)
}

onMounted(load)
</script>

<template>
  <header class="topbar admin-topbar">
    <div class="brand"><CloudIcon /><span>Ycloud · {{ locale.text('管理', 'Admin') }}</span></div>
    <div class="top-actions">
      <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
      <LocaleToggle class="flat" />
      <a class="icon-btn flat" :href="appPath('/browse')" :title="locale.text('返回文件', 'Back to files')" :aria-label="locale.text('返回文件', 'Back to files')">
        <svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" /></svg>
      </a>
    </div>
  </header>

  <main v-if="!requiresLogin" class="admin-shell">
    <aside class="admin-nav glass" :aria-label="locale.text('管理设置', 'Admin settings')">
      <div v-for="group in navigation" :key="group.group" class="admin-nav-group">
        <div class="admin-nav-heading">{{ group.group }}</div>
        <a
          v-for="item in group.items"
          :key="item.id"
          class="admin-nav-item"
          :class="{ active: item.id === activeSection }"
          :href="item.href"
          :aria-current="item.id === activeSection ? 'page' : undefined"
        >
          <AdminNavIcon :name="item.id" />
          <span>{{ item.label }}</span>
        </a>
      </div>
    </aside>
    <div class="admin-content">
      <div v-if="loading" class="admin-loading glass">{{ locale.t('common.loading') }}</div>
      <SecurityView v-else-if="info && activeSection === 'security'" :info="info" @changed="showNotice" />
      <UsersView v-else-if="info && activeSection === 'users'" :info="info" @changed="showNotice" />
      <StorageView
        v-else-if="info && activeSection === 'storage'"
        :instances="info.storage_instances"
        :pending-instance="info.pending_storage_instance"
        :default-storage-id="info.default_storage_id"
        :local-mounts="info.local_mounts ?? []"
        @changed="showNotice"
      />
      <LimitsView v-else-if="info && activeSection === 'limits'" :info="info" @saved="showNotice" />
      <LocksView
        v-else-if="info && activeSection === 'locks'"
        :locks="info.folder_locks"
        :storages="info.storage_instances"
        :default-storage-id="info.default_storage_id"
        @changed="showNotice"
      />
      <WebDavView
        v-else-if="info && activeSection === 'webdav'"
        :mounts="info.shares"
        :storages="info.storage_instances"
        :default-storage-id="info.default_storage_id"
        @changed="showNotice"
      />
      <AccountView v-else-if="info" :info="info" @saved="showNotice" @expired="sessionExpired" />
      <div v-else class="admin-loading glass">{{ loginError || locale.text('管理后台暂时无法加载', 'The admin console is temporarily unavailable') }}</div>
    </div>
  </main>

  <main v-else class="admin-login-shell">
    <form class="admin-login-panel glass" @submit.prevent="submitLogin">
      <CloudIcon class="login-logo" />
      <h1>{{ locale.text('管理员登录', 'Administrator sign-in') }}</h1>
      <label>{{ locale.text('用户名', 'Username') }}<input v-model="username" class="input" autocomplete="username" autofocus></label>
      <label>{{ locale.text('密码', 'Password') }}<input v-model="password" class="input" type="password" autocomplete="current-password"></label>
      <p class="admin-form-error" role="alert">{{ loginError }}</p>
      <button class="btn" type="submit" :disabled="loggingIn">{{ loggingIn ? locale.text('登录中…', 'Signing in…') : locale.text('登录', 'Sign in') }}</button>
    </form>
  </main>

  <div class="toast" :class="{ show: notice }" role="status">{{ notice }}</div>
</template>

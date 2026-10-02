<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, onMounted } from 'vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import AdminLoginCard from '../../shared/components/AdminLoginCard.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import AccountView from './AccountView.vue'
import AdminNavIcon from './AdminNavIcon.vue'
import DashboardView from './DashboardView.vue'
import LimitsView from './LimitsView.vue'
import LocksView from './LocksView.vue'
import ProtectionView from './ProtectionView.vue'
import SecurityView from './SecurityView.vue'
import StorageView from './StorageView.vue'
import WebDavView from './WebDavView.vue'
import UsersView from './UsersView.vue'
import { appPath, currentAppPath } from '../../shared/routes'
import { useAdminSession } from './useAdminSession'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()

const navigation = computed(() => [
  { id: 'dashboard', label: locale.text('仪表盘', 'Dashboard'), href: appPath('/admin/dashboard') },
  { id: 'storage', label: locale.text('存储设置', 'Storage'), href: appPath('/admin/storage') },
  { id: 'webdav', label: 'WebDAV', href: appPath('/admin/webdav') },
  { id: 'locks', label: locale.text('文件夹锁', 'Folder locks'), href: appPath('/admin/locks') },
  { id: 'limits', label: locale.text('传输限制', 'Transfer limits'), href: appPath('/admin/limits') },
  { id: 'account', label: locale.text('管理员设置', 'Administrator settings'), href: appPath('/admin/account') },
  { id: 'users', label: locale.text('用户管理', 'User management'), href: appPath('/admin/users') },
  { id: 'protection', label: locale.text('登录保护', 'Sign-in protection'), href: appPath('/admin/protection') },
  { id: 'security', label: locale.text('访问日志', 'Access logs'), href: appPath('/admin/security') },
] as const)

const activeSection = navigation.value.find(item => item.href === currentAppPath())?.id ?? 'dashboard'
const {
  info, loading, requiresLogin, loadError, username, password, totpCode, totpRequired,
  loginError, loggingIn, notice, noticeRevision, submitLogin, resetTotpChallenge,
  showNotice, sessionExpired, start,
} = useAdminSession(activeSection === 'storage')

onMounted(start)
</script>

<template>
  <main v-if="!requiresLogin" class="admin-shell">
    <header class="admin-header glass">
      <div class="admin-nav-brand"><AppIcon name="cloud" :size="28" /><span>Ycloud {{ locale.text('管理', 'Admin') }}</span></div>
      <div v-if="!loading" class="top-actions">
        <ThemeToggle :theme="theme.current.value" class="flat" @toggle="theme.toggle" />
        <LocaleToggle class="flat" />
        <a class="icon-btn flat" :href="appPath('/browse')" :title="locale.text('返回文件', 'Back to files')" :aria-label="locale.text('返回文件', 'Back to files')">
          <AppIcon name="folder-open" />
        </a>
      </div>
    </header>
    <div class="admin-workspace">
      <aside class="admin-nav glass" :aria-label="locale.text('管理设置', 'Admin settings')">
        <nav class="admin-nav-links">
          <a
            v-for="item in navigation"
            :key="item.id"
            class="admin-nav-item"
            :class="{ active: item.id === activeSection }"
            :href="item.href"
            :aria-current="item.id === activeSection ? 'page' : undefined"
          >
            <AdminNavIcon :name="item.id" />
            <span>{{ item.label }}</span>
          </a>
        </nav>
      </aside>
      <div class="admin-content">
        <div v-if="loading" class="admin-loading glass">{{ locale.t('common.loading') }}</div>
        <DashboardView v-else-if="info && activeSection === 'dashboard'" :info="info" />
        <SecurityView v-else-if="info && activeSection === 'security'" :info="info" @changed="showNotice" />
        <ProtectionView v-else-if="info && activeSection === 'protection'" :info="info" @saved="showNotice" />
        <UsersView v-else-if="info && activeSection === 'users'" :info="info" @changed="showNotice" />
        <StorageView
          v-else-if="info && activeSection === 'storage'"
          :instances="info.storage_instances"
          :pending-instance="info.pending_storage_instance"
          :local-mounts="info.local_mounts ?? []"
          @changed="showNotice"
        />
        <LimitsView v-else-if="info && activeSection === 'limits'" :info="info" @saved="showNotice" />
        <LocksView
          v-else-if="info && activeSection === 'locks'"
          :locks="info.folder_locks"
          :storages="info.storage_instances"
          @changed="showNotice"
        />
        <WebDavView
          v-else-if="info && activeSection === 'webdav'"
          :mounts="info.shares"
          :storages="info.storage_instances"
          @changed="showNotice"
        />
        <AccountView v-else-if="info" :info="info" @saved="showNotice" @expired="sessionExpired" />
        <div v-else class="admin-loading glass">{{ loadError || locale.text('管理后台暂时无法加载', 'The admin console is temporarily unavailable') }}</div>
      </div>
    </div>
  </main>

  <main v-else class="admin-login-shell">
    <AdminLoginCard
      v-model:username="username"
      v-model:password="password"
      v-model:totp-code="totpCode"
      :totp-required="totpRequired"
      :error="loginError"
      :busy="loggingIn"
      @credentials-change="resetTotpChallenge"
      @submit="submitLogin"
    />
  </main>

  <AppFeedback :message="notice" :revision="noticeRevision" kind="success" />
  <AppFeedback :message="loadError" />
</template>

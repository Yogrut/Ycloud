<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import type { AdminInfo, UpdateAccountRequest, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'
import { updateAccount, updateLoginSecuritySettings } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const MASK = '••••••'
const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string]; expired: [] }>()
const locale = useLocale()

const username = ref('')
const adminPassword = ref(MASK)
const webPassword = ref('')
const saving = ref(false)
const errorMessage = ref('')
const confirmRemoval = ref(false)
const adminFailures = ref<string | number>('')
const adminBlockMinutes = ref<string | number>('')
const webFailures = ref<string | number>('')
const webBlockMinutes = ref<string | number>('')
const savingSecurity = ref(false)
const securityError = ref('')

const hasAccountChanges = computed(() => (
  username.value.trim() !== props.info.username
  || adminPassword.value !== MASK
  || webPassword.value !== (props.info.has_global_web_password ? MASK : '')
))

const hasSecurityChanges = computed(() => (
  Number(adminFailures.value) !== props.info.admin_login_failures
  || Number(adminBlockMinutes.value) * 60 !== props.info.admin_login_block_seconds
  || Number(webFailures.value) !== props.info.web_login_failures
  || Number(webBlockMinutes.value) * 60 !== props.info.web_login_block_seconds
))

function reset(): void {
  username.value = props.info.username
  adminPassword.value = MASK
  webPassword.value = props.info.has_global_web_password ? MASK : ''
  errorMessage.value = ''
  confirmRemoval.value = false
  adminFailures.value = props.info.admin_login_failures
  adminBlockMinutes.value = props.info.admin_login_block_seconds / 60
  webFailures.value = props.info.web_login_failures
  webBlockMinutes.value = props.info.web_login_block_seconds / 60
  securityError.value = ''
}

watch(() => props.info, reset, { immediate: true })

function selectMask(event: FocusEvent): void {
  const input = event.target as HTMLInputElement
  if (input.value === MASK) nextTick(() => input.select())
}

function validate(): string | undefined {
  if (!username.value.trim()) return locale.text('管理员用户名不能为空', 'Administrator username is required')
  if (adminPassword.value !== MASK && [...adminPassword.value].length < 12) return locale.text('管理员密码至少需要 12 位', 'Administrator password must be at least 12 characters')
  if (webPassword.value !== MASK && webPassword.value && [...webPassword.value].length < 8) return locale.text('网页访问密码至少需要 8 位', 'Browser access password must be at least 8 characters')
  return undefined
}

async function submit(confirmed = false): Promise<void> {
  if (saving.value || !hasAccountChanges.value) return
  const validationError = validate()
  if (validationError) {
    errorMessage.value = validationError
    return
  }
  if (!confirmed && props.info.has_global_web_password && webPassword.value === '') {
    confirmRemoval.value = true
    return
  }

  const body: UpdateAccountRequest = {}
  const normalizedUsername = username.value.trim()
  const credentialsChanged = normalizedUsername !== props.info.username || adminPassword.value !== MASK
  if (normalizedUsername !== props.info.username) body.username = normalizedUsername
  if (adminPassword.value !== MASK) body.password = adminPassword.value
  if (webPassword.value !== (props.info.has_global_web_password ? MASK : '')) body.global_web_password = webPassword.value

  saving.value = true
  errorMessage.value = ''
  confirmRemoval.value = false
  try {
    const result = await updateAccount(body)
    const message = result.warning ?? (credentialsChanged
      ? locale.text('账户已更新，请重新登录', 'Account updated. Sign in again')
      : locale.text('账户设置已更新', 'Account settings updated'))
    emit('saved', message)
    if (credentialsChanged) emit('expired')
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    saving.value = false
  }
}

function securityRequest(): UpdateLoginSecuritySettingsRequest {
  const adminAttempts = Number(adminFailures.value)
  const webAttempts = Number(webFailures.value)
  const adminMinutes = Number(adminBlockMinutes.value)
  const webMinutes = Number(webBlockMinutes.value)
  if (!Number.isInteger(adminAttempts) || adminAttempts < 3 || adminAttempts > 10) {
    throw new Error(locale.text('管理员错误次数必须在 3 到 10 之间', 'Administrator failures must be between 3 and 10'))
  }
  if (!Number.isInteger(webAttempts) || webAttempts < 3 || webAttempts > 20) {
    throw new Error(locale.text('首页错误次数必须在 3 到 20 之间', 'Browser failures must be between 3 and 20'))
  }
  if (!Number.isInteger(adminMinutes) || adminMinutes < 5 || adminMinutes > 1440
    || !Number.isInteger(webMinutes) || webMinutes < 5 || webMinutes > 1440) {
    throw new Error(locale.text('封禁时间必须在 5 到 1440 分钟之间', 'Block duration must be between 5 and 1440 minutes'))
  }
  return {
    admin_login_failures: adminAttempts,
    web_login_failures: webAttempts,
    admin_login_block_seconds: adminMinutes * 60,
    web_login_block_seconds: webMinutes * 60,
  }
}

async function submitSecurity(): Promise<void> {
  if (savingSecurity.value || !hasSecurityChanges.value) return
  let body: UpdateLoginSecuritySettingsRequest
  try {
    body = securityRequest()
  } catch (error) {
    securityError.value = error instanceof Error ? error.message : locale.text('登录保护设置无效', 'Invalid sign-in protection settings')
    return
  }
  savingSecurity.value = true
  securityError.value = ''
  try {
    await updateLoginSecuritySettings(body)
    emit('saved', locale.text('登录保护设置已保存', 'Sign-in protection settings saved'))
  } catch (error) {
    securityError.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    savingSecurity.value = false
  }
}
</script>

<template>
  <section class="admin-pane glass" aria-labelledby="account-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="account-title">{{ locale.text('账户与访问', 'Account & access') }}</h1>
        <p>{{ locale.text('分别管理后台管理员凭据和首页访问密码。', 'Manage administrator credentials and the browser access password separately.') }}</p>
      </div>
    </header>
    <form class="admin-pane-body" @submit.prevent="submit()">
      <div class="account-form">
        <label class="admin-field">
          <span>{{ locale.text('管理员用户名', 'Administrator username') }}</span>
          <input v-model="username" autocomplete="username" maxlength="128">
        </label>
        <label class="admin-field">
          <span>{{ locale.text('管理员密码', 'Administrator password') }}</span>
          <input v-model="adminPassword" type="password" autocomplete="new-password" minlength="12" @focus="selectMask">
        </label>
        <label class="admin-field">
          <span>{{ locale.text('网页访问密码', 'Browser access password') }}</span>
          <input v-model="webPassword" type="password" autocomplete="new-password" @focus="selectMask">
        </label>
      </div>
      <p class="admin-form-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
      <div class="admin-save-row">
        <button class="btn" type="submit" :disabled="saving || !hasAccountChanges">{{ saving ? locale.t('common.saving') : locale.text('保存账户设置', 'Save account settings') }}</button>
      </div>
    </form>
  </section>

  <section class="admin-pane glass" aria-labelledby="login-policy-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="login-policy-title">{{ locale.text('登录保护', 'Sign-in protection') }}</h1>
        <p>{{ locale.text('管理员和首页分别计数；WebDAV 固定为 5 次错误后限制 60 秒。', 'Administrator and browser failures are counted separately. WebDAV remains fixed at 5 failures and a 60-second restriction.') }}</p>
      </div>
    </header>
    <form class="admin-pane-body" @submit.prevent="submitSecurity">
      <div class="account-form">
        <label class="admin-field limits-field">
          <span>{{ locale.text('管理员错误次数', 'Administrator failure limit') }}</span>
          <span class="limits-control"><input v-model="adminFailures" type="number" min="3" max="10" step="1"><small>3–10</small></span>
        </label>
        <label class="admin-field limits-field">
          <span>{{ locale.text('管理员封禁时间', 'Administrator block duration') }}</span>
          <span class="limits-control"><span class="input-with-unit"><input v-model="adminBlockMinutes" type="number" min="5" max="1440" step="1"><span>{{ locale.text('分钟', 'min') }}</span></span><small>5–1440</small></span>
        </label>
        <label class="admin-field limits-field">
          <span>{{ locale.text('首页错误次数', 'Browser failure limit') }}</span>
          <span class="limits-control"><input v-model="webFailures" type="number" min="3" max="20" step="1"><small>3–20</small></span>
        </label>
        <label class="admin-field limits-field">
          <span>{{ locale.text('首页封禁时间', 'Browser block duration') }}</span>
          <span class="limits-control"><span class="input-with-unit"><input v-model="webBlockMinutes" type="number" min="5" max="1440" step="1"><span>{{ locale.text('分钟', 'min') }}</span></span><small>5–1440</small></span>
        </label>
      </div>
      <p class="admin-form-error" role="alert">{{ securityError }}</p>
      <div class="admin-save-row"><button class="btn" type="submit" :disabled="savingSecurity || !hasSecurityChanges">{{ savingSecurity ? locale.t('common.saving') : locale.text('保存登录保护', 'Save sign-in protection') }}</button></div>
    </form>
  </section>

  <div v-if="confirmRemoval" class="overlay" @click.self="confirmRemoval = false">
    <section class="modal" role="dialog" aria-modal="true" aria-labelledby="remove-gate-title">
      <h2 id="remove-gate-title">{{ locale.text('确认移除首页密码', 'Remove browser access password?') }}</h2>
      <p>{{ locale.text('移除后，任何能连接服务器的人都可进入文件浏览界面；文件夹锁仍然有效。', 'Anyone who can reach the server will be able to open the file browser. Folder locks will remain active.') }}</p>
      <div class="modal-actions">
        <button class="btn secondary" type="button" @click="confirmRemoval = false">{{ locale.t('common.cancel') }}</button>
        <button class="btn danger" type="button" @click="submit(true)">{{ locale.text('确认移除', 'Remove password') }}</button>
      </div>
    </section>
  </div>
</template>

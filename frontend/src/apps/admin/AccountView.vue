<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import type { AdminInfo, UpdateAccountRequest } from '../../shared/api/admin'
import {
  disableAdministratorTotp,
  enableAdministratorTotp,
  setupAdministratorTotp,
  updateAccount,
} from '../../shared/api/admin'
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
const totpEnabled = ref(Boolean(props.info.admin_totp_enabled))
const totpPassword = ref('')
const totpCode = ref('')
const totpSetup = ref<{ secret: string; provisioning_uri: string; qr_svg: string }>()
const recoveryCodes = ref<string[]>([])
const totpBusy = ref(false)
const totpError = ref('')

const hasAccountChanges = computed(() => (
  username.value.trim() !== props.info.username
  || adminPassword.value !== MASK
  || webPassword.value !== (props.info.has_global_web_password ? MASK : '')
))

function reset(): void {
  username.value = props.info.username
  adminPassword.value = MASK
  webPassword.value = props.info.has_global_web_password ? MASK : ''
  errorMessage.value = ''
  confirmRemoval.value = false
}

watch(() => props.info, reset, { immediate: true })
watch(() => props.info.admin_totp_enabled, value => {
  if (recoveryCodes.value.length === 0) totpEnabled.value = Boolean(value)
})

function selectMask(event: FocusEvent): void {
  const input = event.target as HTMLInputElement
  if (input.value === MASK) nextTick(() => input.select())
}

function qrDataUrl(svg: string): string {
  return `data:image/svg+xml;base64,${window.btoa(svg)}`
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

async function beginTotpSetup(): Promise<void> {
  if (!totpPassword.value || totpBusy.value) return
  totpBusy.value = true
  totpError.value = ''
  try {
    totpSetup.value = await setupAdministratorTotp(totpPassword.value)
  } catch (error) {
    totpError.value = error instanceof Error ? error.message : locale.text('无法创建两步验证配置', 'Unable to prepare two-step verification')
  } finally {
    totpBusy.value = false
  }
}

async function enableTotp(): Promise<void> {
  if (!totpSetup.value || !totpPassword.value || !totpCode.value || totpBusy.value) return
  totpBusy.value = true
  totpError.value = ''
  try {
    const result = await enableAdministratorTotp(totpPassword.value, totpSetup.value.secret, totpCode.value.trim())
    recoveryCodes.value = result.recovery_codes
    totpEnabled.value = true
    totpSetup.value = undefined
    totpCode.value = ''
    totpPassword.value = ''
  } catch (error) {
    totpError.value = error instanceof Error ? error.message : locale.text('启用两步验证失败', 'Unable to enable two-step verification')
  } finally {
    totpBusy.value = false
  }
}

async function disableTotp(): Promise<void> {
  if (!totpPassword.value || !totpCode.value || totpBusy.value) return
  totpBusy.value = true
  totpError.value = ''
  try {
    await disableAdministratorTotp(totpPassword.value, totpCode.value.trim())
    totpEnabled.value = false
    totpPassword.value = ''
    totpCode.value = ''
    emit('saved', locale.text('两步验证已停用，请重新登录', 'Two-step verification disabled. Sign in again'))
    emit('expired')
  } catch (error) {
    totpError.value = error instanceof Error ? error.message : locale.text('停用两步验证失败', 'Unable to disable two-step verification')
  } finally {
    totpBusy.value = false
  }
}

</script>

<template>
  <section class="admin-pane form-pane account-pane glass" aria-labelledby="account-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="account-title">{{ locale.text('管理员设置', 'Administrator settings') }}</h1>
        <p>{{ locale.text('管理管理员凭据和文件首页的访问密码。', 'Manage administrator credentials and the file browser access password.') }}</p>
      </div>
    </header>
    <div class="admin-pane-body">
      <form class="account-section account-credentials" :aria-label="locale.text('账户设置', 'Account settings')" @submit.prevent="submit()">
        <div class="settings-grid account-grid">
          <label class="compact-field">
            <span>{{ locale.text('管理员用户名', 'Administrator username') }}</span>
            <input v-model="username" autocomplete="username" maxlength="128">
          </label>
          <label class="compact-field">
            <span>{{ locale.text('管理员密码', 'Administrator password') }}</span>
            <input v-model="adminPassword" type="password" autocomplete="new-password" minlength="12" @focus="selectMask">
          </label>
          <label class="compact-field">
            <span>{{ locale.text('网页访问密码', 'Browser access password') }}</span>
            <input v-model="webPassword" type="password" autocomplete="new-password" @focus="selectMask">
          </label>
        </div>
        <p class="admin-form-error" role="alert" aria-live="polite">{{ errorMessage }}</p>
        <div class="admin-save-row">
          <button class="btn" type="submit" :disabled="saving || !hasAccountChanges">{{ saving ? locale.t('common.saving') : locale.text('保存账户设置', 'Save account settings') }}</button>
        </div>
      </form>
      <section class="account-section totp-section" :aria-label="locale.text('管理员两步验证', 'Administrator two-step verification')">
        <div class="section-heading totp-heading">
          <div>
            <h2>{{ locale.text('两步验证', 'Two-step verification') }}</h2>
            <p>{{ locale.text('使用 2FAuth 或其他标准验证器；服务器时间需保持准确。', 'Use 2FAuth or another standard authenticator; keep the server clock accurate.') }}</p>
          </div>
          <span class="status-pill" :class="{ inactive: !totpEnabled }">{{ totpEnabled ? locale.text('已启用', 'Enabled') : locale.text('未启用', 'Disabled') }}</span>
        </div>

        <div v-if="recoveryCodes.length" class="settings-grid">
          <div class="compact-field">
            <span>{{ locale.text('恢复码（仅显示一次）', 'Recovery codes (shown once)') }}</span>
            <code v-for="code in recoveryCodes" :key="code">{{ code }}</code>
            <small>{{ locale.text('立即离线保存。每个恢复码只能使用一次，然后请重新登录。', 'Save these offline now. Each code works once, then sign in again.') }}</small>
          </div>
        </div>

        <template v-else>
          <div class="settings-grid account-grid">
            <label class="compact-field">
              <span>{{ locale.text('当前管理员密码', 'Current administrator password') }}</span>
              <input v-model="totpPassword" type="password" autocomplete="current-password">
            </label>
            <label v-if="totpEnabled || totpSetup" class="compact-field">
              <span>{{ locale.text('动态验证码或恢复码', 'Authenticator or recovery code') }}</span>
              <input v-model="totpCode" inputmode="numeric" autocomplete="one-time-code" maxlength="16">
            </label>
          </div>
          <div v-if="totpSetup" class="settings-grid totp-setup-grid">
            <div class="totp-qr-card">
              <img :src="qrDataUrl(totpSetup.qr_svg)" :alt="locale.text('两步验证二维码', 'Two-step verification QR code')">
              <small>{{ locale.text('使用 2FAuth 扫描二维码', 'Scan with 2FAuth') }}</small>
            </div>
            <label class="compact-field totp-secret-field">
              <span>{{ locale.text('无法扫码时手动输入密钥', 'Enter the secret manually if scanning is unavailable') }}</span>
              <input :value="totpSetup.secret" readonly>
            </label>
          </div>
          <p class="admin-form-error" role="alert" aria-live="polite">{{ totpError }}</p>
          <div class="admin-save-row">
            <button v-if="!totpEnabled && !totpSetup" class="btn" type="button" :disabled="totpBusy || !totpPassword" @click="beginTotpSetup">{{ locale.text('开始设置', 'Set up') }}</button>
            <button v-else-if="totpSetup" class="btn" type="button" :disabled="totpBusy || !totpCode" @click="enableTotp">{{ locale.text('验证并启用', 'Verify and enable') }}</button>
            <button v-else class="btn danger" type="button" :disabled="totpBusy || !totpPassword || !totpCode" @click="disableTotp">{{ locale.text('验证并停用', 'Verify and disable') }}</button>
          </div>
        </template>
      </section>
    </div>
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

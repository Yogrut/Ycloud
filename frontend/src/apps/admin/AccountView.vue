<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import { computed, nextTick, ref, watch } from 'vue'
import SettingRow from '../../shared/components/SettingRow.vue'
import DomainBindingSetting from './DomainBindingSetting.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
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
const feedbackRevision = ref(0)
const copyNotice = ref<{ message: string; kind: 'success' | 'error' }>()

async function copyTotpSecret(): Promise<void> {
  if (!totpSetup.value) return
  const secret = totpSetup.value.secret
  copyNotice.value = undefined
  try {
    await navigator.clipboard.writeText(secret)
    if (totpSetup.value?.secret !== secret) return
    copyNotice.value = { message: locale.text('密钥已复制', 'Secret copied'), kind: 'success' }
  } catch {
    if (totpSetup.value?.secret !== secret) return
    copyNotice.value = { message: locale.text('无法复制，请使用二维码或手动记录密钥', 'Unable to copy. Scan the QR code or transcribe the secret.'), kind: 'error' }
  }
}

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
const editor = ref<'username' | 'adminPassword' | 'webPassword' | 'totp'>()
const settingLabels = computed(() => ({
  username: locale.text('管理员用户名', 'Administrator username'),
  adminPassword: locale.text('管理员密码', 'Administrator password'),
  webPassword: locale.text('网页访问密码', 'Browser access password'),
  totp: locale.text('两步验证', 'Two-step verification'),
}))
function openEditor(key: NonNullable<typeof editor.value>): void {
  copyNotice.value = undefined
  reset()
  editor.value = key
}
function closeEditor(): void {
  if (saving.value || totpBusy.value) return
  copyNotice.value = undefined
  if (recoveryCodes.value.length) {
    emit('saved', locale.text('两步验证已启用，请重新登录', 'Two-step verification enabled. Sign in again'))
    emit('expired')
  }
  editor.value = undefined
  totpPassword.value = ''
  totpCode.value = ''
  totpSetup.value = undefined
  recoveryCodes.value = []
  totpError.value = ''
  reset()
}

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
    editor.value = undefined
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
    <div class="settings-rows">
      <SettingRow :label="settingLabels.username" :value="info.username" @edit="openEditor('username')" />
      <SettingRow :label="settingLabels.adminPassword" value="••••••" @edit="openEditor('adminPassword')" />
      <SettingRow :label="settingLabels.webPassword" :value="info.has_global_web_password ? '••••••' : locale.text('未设置', 'Not set')" @edit="openEditor('webPassword')" />
      <SettingRow :label="settingLabels.totp" :value="totpEnabled ? locale.text('已启用', 'Enabled') : locale.text('未启用', 'Disabled')" @edit="openEditor('totp')" />
      <DomainBindingSetting :initial="info.domain_binding" />
    </div>
  </section>
  <SettingsDrawer v-if="editor" :title="settingLabels[editor]" :busy="saving || totpBusy || confirmRemoval" @close="closeEditor">
    <form v-if="editor !== 'totp'" class="drawer-form account-credentials" :aria-label="locale.text('账户设置', 'Account settings')" @submit.prevent="feedbackRevision++; submit()">
      <div class="settings-grid account-grid">
        <label v-if="editor === 'username'" class="compact-field">
          <span>{{ locale.text('管理员用户名', 'Administrator username') }}</span>
          <input v-model="username" aria-required="true" autocomplete="username" maxlength="128">
        </label>
        <label v-if="editor === 'adminPassword'" class="compact-field">
          <span>{{ locale.text('管理员密码', 'Administrator password') }}</span>
          <input v-model="adminPassword" aria-required="true" type="password" autocomplete="new-password" minlength="12" @focus="selectMask">
        </label>
        <label v-if="editor === 'webPassword'" class="compact-field">
          <span>{{ locale.text('网页访问密码', 'Browser access password') }}</span>
          <input v-model="webPassword" type="password" autocomplete="new-password" @focus="selectMask">
        </label>
      </div>
      <AppFeedback :revision="feedbackRevision" :message="errorMessage" />
      <div class="modal-actions">
        <button class="btn secondary" type="button" :disabled="saving" @click="closeEditor">{{ locale.t('common.cancel') }}</button>
        <button class="btn" type="submit" :disabled="saving || !hasAccountChanges">{{ locale.text('确认', 'Confirm') }}</button>
      </div>
    </form>
    <section v-else class="drawer-form totp-section" :aria-label="locale.text('管理员两步验证', 'Administrator two-step verification')">
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
            <input v-model="totpPassword" aria-required="true" type="password" autocomplete="current-password">
          </label>
          <label v-if="totpEnabled || totpSetup" class="compact-field">
            <span>{{ locale.text('动态验证码或恢复码', 'Authenticator or recovery code') }}</span>
            <input v-model="totpCode" aria-required="true" inputmode="numeric" autocomplete="one-time-code" maxlength="16">
          </label>
        </div>
        <div v-if="totpSetup" class="settings-grid totp-setup-grid">
          <div class="totp-qr-card">
            <img :src="qrDataUrl(totpSetup.qr_svg)" :alt="locale.text('两步验证二维码', 'Two-step verification QR code')">
            <small>{{ locale.text('使用 2FAuth 扫描二维码', 'Scan with 2FAuth') }}</small>
          </div>
          <label class="compact-field totp-secret-field">
            <span>{{ locale.text('无法扫码时手动输入密钥', 'Enter the secret manually if scanning is unavailable') }}</span>
            <span class="readonly-copy-field">
              <input :value="totpSetup.secret" disabled>
              <button type="button" :title="locale.text('复制密钥', 'Copy secret')" :aria-label="locale.text('复制密钥', 'Copy secret')" @click="copyTotpSecret"><AppIcon name="copy" :size="16" weight="regular" /></button>
            </span>
          </label>
        </div>
        <AppFeedback :revision="feedbackRevision" :message="totpError" />
        <AppFeedback v-if="copyNotice" :message="copyNotice.message" :kind="copyNotice.kind" />
        <p class="field-hint">{{ totpEnabled ? locale.text('验证后停用两步验证。', 'Verify to disable two-step verification.') : totpSetup ? locale.text('输入验证码后启用两步验证。', 'Enter the code to enable two-step verification.') : locale.text('确认后生成验证器绑定信息。', 'Confirm to generate authenticator setup details.') }}</p>
        <div class="admin-save-row">
          <button class="btn secondary" type="button" :disabled="totpBusy" @click="closeEditor">{{ locale.t('common.cancel') }}</button>
          <button v-if="!totpEnabled && !totpSetup" class="btn" type="button" :disabled="totpBusy || !totpPassword" @click="beginTotpSetup">{{ locale.text('确认', 'Confirm') }}</button>
          <button v-else-if="totpSetup" class="btn" type="button" :disabled="totpBusy || !totpCode" @click="enableTotp">{{ locale.text('确认', 'Confirm') }}</button>
          <button v-else class="btn danger" type="button" :disabled="totpBusy || !totpPassword || !totpCode" @click="disableTotp">{{ locale.text('确认', 'Confirm') }}</button>
        </div>
      </template>
      <div v-if="recoveryCodes.length" class="modal-actions"><button class="btn" type="button" @click="closeEditor">{{ locale.text('确认', 'Confirm') }}</button></div>
    </section>
  </SettingsDrawer>

  <ConfirmDialog
    v-if="confirmRemoval"
    :title="locale.text('确认移除首页密码', 'Remove browser access password?')"
    :message="locale.text('移除后，任何能连接服务器的人都可进入文件浏览界面；文件夹锁仍然有效。', 'Anyone who can reach the server will be able to open the file browser. Folder locks will remain active.')"
    :busy="saving" @close="confirmRemoval = false" @confirm="submit(true)"
  />
</template>

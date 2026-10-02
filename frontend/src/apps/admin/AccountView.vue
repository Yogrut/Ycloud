<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import ConfirmDialog from '../../shared/components/ConfirmDialog.vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import { computed, ref, watch } from 'vue'
import SettingRow from '../../shared/components/SettingRow.vue'
import DomainBindingSetting from './DomainBindingSetting.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import type { AdminInfo, UpdateAccountRequest } from '../../shared/api/admin'
import { updateAccount } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { selectStoredPassword as selectMask } from '../../shared/passwordInput'
import { useAdministratorTotp } from './useAdministratorTotp'
import { ADMIN_ACCOUNT_PASSWORD_MINIMUMS, adminAccountDraft, adminAccountChanges, buildAdminAccountRequest } from './adminAccountForm'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string]; expired: [] }>()
const locale = useLocale()
const feedbackRevision = ref(0)

const draft = ref(adminAccountDraft(props.info))
const saving = ref(false)
const errorMessage = ref('')
const confirmRemoval = ref(false)
const totp = useAdministratorTotp(() => props.info.admin_totp_enabled, () => {
  emit('saved', locale.text('两步验证已停用，请重新登录', 'Two-step verification disabled. Sign in again'))
  emit('expired')
})
const {
  enabled: totpEnabled, password: totpPassword, code: totpCode, setup: totpSetup,
  recoveryCodes, busy: totpBusy, error: totpError, copyNotice,
  copySecret: copyTotpSecret, beginSetup: beginTotpSetup, enable: enableTotp, disable: disableTotp,
} = totp
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
  totp.reset()
  reset()
}

const accountChanges = computed(() => adminAccountChanges(draft.value, props.info))
const hasAccountChanges = computed(() => Object.keys(accountChanges.value).length > 0)

function reset(): void {
  draft.value = adminAccountDraft(props.info)
  errorMessage.value = ''
  confirmRemoval.value = false
}

watch(() => props.info, reset, { immediate: true })

function qrDataUrl(svg: string): string {
  return `data:image/svg+xml;base64,${window.btoa(svg)}`
}

async function submit(confirmed = false): Promise<void> {
  if (saving.value || !hasAccountChanges.value) return
  let body: UpdateAccountRequest
  try {
    body = buildAdminAccountRequest(draft.value, props.info, locale.text)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
    return
  }
  if (!confirmed && body.global_web_password === '') {
    confirmRemoval.value = true
    return
  }

  const credentialsChanged = body.username !== undefined || body.password !== undefined

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
          <input v-model="draft.username" aria-required="true" autocomplete="username" maxlength="128">
        </label>
        <label v-if="editor === 'adminPassword'" class="compact-field">
          <span>{{ locale.text('管理员密码', 'Administrator password') }}</span>
          <input v-model="draft.adminPassword" aria-required="true" type="password" autocomplete="new-password" :minlength="ADMIN_ACCOUNT_PASSWORD_MINIMUMS.administrator" @focus="selectMask">
        </label>
        <label v-if="editor === 'webPassword'" class="compact-field">
          <span>{{ locale.text('网页访问密码', 'Browser access password') }}</span>
          <input v-model="draft.webPassword" type="password" autocomplete="new-password" @focus="selectMask">
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

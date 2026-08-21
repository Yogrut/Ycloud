<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import type { AdminInfo, UpdateAccountRequest } from '../../shared/api/admin'
import { updateAccount } from '../../shared/api/admin'
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

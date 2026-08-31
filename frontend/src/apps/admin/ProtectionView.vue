<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { AdminInfo, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'
import { updateLoginSecuritySettings } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()
const adminFailures = ref<string | number>('')
const adminBlockMinutes = ref<string | number>('')
const webFailures = ref<string | number>('')
const webBlockMinutes = ref<string | number>('')
const saving = ref(false)
const errorMessage = ref('')

const hasChanges = computed(() => (
  Number(adminFailures.value) !== props.info.admin_login_failures
  || Number(adminBlockMinutes.value) * 60 !== props.info.admin_login_block_seconds
  || Number(webFailures.value) !== props.info.web_login_failures
  || Number(webBlockMinutes.value) * 60 !== props.info.web_login_block_seconds
))

function reset(): void {
  adminFailures.value = props.info.admin_login_failures
  adminBlockMinutes.value = props.info.admin_login_block_seconds / 60
  webFailures.value = props.info.web_login_failures
  webBlockMinutes.value = props.info.web_login_block_seconds / 60
  errorMessage.value = ''
}

watch(() => props.info, reset, { immediate: true })

function requestBody(): UpdateLoginSecuritySettingsRequest {
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

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  let body: UpdateLoginSecuritySettingsRequest
  try {
    body = requestBody()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('登录保护设置无效', 'Invalid sign-in protection settings')
    return
  }
  saving.value = true
  errorMessage.value = ''
  try {
    await updateLoginSecuritySettings(body)
    emit('saved', locale.text('登录保护设置已保存', 'Sign-in protection settings saved'))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <section class="admin-pane form-pane glass" aria-labelledby="protection-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="protection-title">{{ locale.text('登录保护', 'Sign-in protection') }}</h1>
        <p>{{ locale.text('设置连续登录失败后的自动封禁阈值和封禁时间。', 'Set the automatic restriction threshold and duration after consecutive sign-in failures.') }}</p>
      </div>
    </header>
    <form class="admin-pane-body" @submit.prevent="submit">
      <div class="settings-grid security-settings-grid">
        <label class="compact-field limits-field"><span>{{ locale.text('管理员错误次数', 'Administrator failure limit') }}</span><span class="limits-control"><input v-model="adminFailures" type="number" min="3" max="10" step="1"><small>3–10</small></span></label>
        <label class="compact-field limits-field"><span>{{ locale.text('管理员封禁时间', 'Administrator block duration') }}</span><span class="limits-control"><span class="input-with-unit"><input v-model="adminBlockMinutes" type="number" min="5" max="1440" step="1"><span>{{ locale.text('分钟', 'min') }}</span></span><small>5–1440</small></span></label>
        <label class="compact-field limits-field"><span>{{ locale.text('首页错误次数', 'Browser failure limit') }}</span><span class="limits-control"><input v-model="webFailures" type="number" min="3" max="20" step="1"><small>3–20</small></span></label>
        <label class="compact-field limits-field"><span>{{ locale.text('首页封禁时间', 'Browser block duration') }}</span><span class="limits-control"><span class="input-with-unit"><input v-model="webBlockMinutes" type="number" min="5" max="1440" step="1"><span>{{ locale.text('分钟', 'min') }}</span></span><small>5–1440</small></span></label>
      </div>
      <p class="admin-form-error" role="alert">{{ errorMessage }}</p>
      <p class="account-field-note">{{ locale.text('管理员和首页分别计数；WebDAV 固定为 5 次错误后限制 60 秒。', 'Administrator and browser failures are counted separately. WebDAV remains fixed at 5 failures and a 60-second restriction.') }}</p>
      <div class="admin-save-row"><button class="btn" type="submit" :disabled="saving || !hasChanges">{{ saving ? locale.t('common.saving') : locale.text('保存登录保护', 'Save sign-in protection') }}</button></div>
    </form>
  </section>
</template>

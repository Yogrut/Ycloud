<script setup lang="ts">
import AppIcon from './AppIcon.vue'
import { useLocale } from '../i18n'

const props = withDefaults(defineProps<{
  username: string
  password: string
  totpCode: string
  totpRequired: boolean
  error?: string
  busy?: boolean
  cancelable?: boolean
}>(), {
  error: '',
  busy: false,
  cancelable: false,
})

const emit = defineEmits<{
  'update:username': [value: string]
  'update:password': [value: string]
  'update:totpCode': [value: string]
  'credentials-change': []
  submit: []
  cancel: []
}>()

const locale = useLocale()

function updateUsername(event: Event): void {
  emit('update:username', (event.target as HTMLInputElement).value)
  emit('credentials-change')
}

function updatePassword(event: Event): void {
  emit('update:password', (event.target as HTMLInputElement).value)
  emit('credentials-change')
}

function updateTotpCode(event: Event): void {
  emit('update:totpCode', (event.target as HTMLInputElement).value)
}
</script>

<template>
  <form class="admin-login-card" @submit.prevent="emit('submit')">
    <button
      v-if="cancelable"
      class="admin-login-close"
      type="button"
      :aria-label="locale.t('common.cancel')"
      :title="locale.t('common.cancel')"
      @click="emit('cancel')"
    >
      ×
    </button>

    <section class="admin-login-brand">
      <div class="admin-login-lockup">
        <span class="admin-login-logo"><AppIcon name="cloud" :size="34" /></span>
        <strong>Ycloud</strong>
        <span>ADMIN</span>
      </div>
      <div class="admin-login-intro">
        <p class="admin-login-kicker">{{ locale.text('统一存储管理', 'Unified storage management') }}</p>
        <h1>{{ locale.text('统一入口，管理所有存储', 'One clear entrance for every storage') }}</h1>
        <p>{{ locale.text('统一接入本地存储与对象存储，在明确的权限边界内管理文件。', 'Connect local and object storage under clear permission boundaries.') }}</p>
      </div>
      <div class="admin-login-map" aria-hidden="true">
        <span class="admin-login-connector top-center" />
        <span class="admin-login-connector bottom-left" />
        <span class="admin-login-connector bottom-right" />
        <div class="admin-login-platform"><AppIcon name="cloud" :size="18" /><span>Ycloud</span></div>
        <div class="admin-login-hub"><AppIcon name="lock" :size="18" /><span>{{ locale.text('权限控制', 'Access control') }}</span></div>
        <div class="admin-login-storage-row">
          <div class="admin-login-node"><AppIcon name="storage" :size="18" /><span>{{ locale.text('本地存储', 'Local') }}</span></div>
          <div class="admin-login-node"><AppIcon name="cloud" :size="18" /><span>{{ locale.text('S3 存储', 'S3') }}</span></div>
        </div>
      </div>
    </section>

    <section class="admin-login-form-pane">
      <header class="admin-login-form-head">
        <span>{{ locale.text('Ycloud 管理后台', 'Ycloud administration') }}</span>
        <h2>{{ locale.text('账号登录', 'Account sign-in') }}</h2>
        <p>{{ locale.text('使用已授权的账号继续访问。', 'Continue with an authorized account.') }}</p>
      </header>

      <label>
        <span>{{ locale.text('用户名', 'Username') }}</span>
        <input :value="props.username" class="input" autocomplete="username" autofocus @input="updateUsername">
      </label>
      <label>
        <span>{{ locale.text('密码', 'Password') }}</span>
        <input :value="props.password" class="input" type="password" autocomplete="current-password" @input="updatePassword">
      </label>
      <label v-if="totpRequired">
        <span>{{ locale.text('动态验证码或恢复码', 'Authenticator or recovery code') }}</span>
        <input :value="props.totpCode" class="input" inputmode="numeric" autocomplete="one-time-code" maxlength="16" autofocus @input="updateTotpCode">
      </label>

      <p class="admin-login-error" role="alert" aria-live="polite">{{ error }}</p>
      <button class="btn admin-login-submit" type="submit" :disabled="busy || (totpRequired && !totpCode.trim())">
        {{ busy ? locale.text('登录中…', 'Signing in…') : (totpRequired ? locale.text('验证并登录', 'Verify and sign in') : locale.text('登录', 'Sign in')) }}
      </button>
    </section>
  </form>
</template>

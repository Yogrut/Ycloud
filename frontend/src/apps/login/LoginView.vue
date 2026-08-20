<script setup lang="ts">
import { nextTick, onMounted, ref } from 'vue'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import { enterGate, getIdentity } from '../../shared/api/auth'

const password = ref('')
const errorMessage = ref('')
const submitting = ref(false)
const passwordInput = ref<HTMLInputElement>()

function openBrowser(): void {
  sessionStorage.removeItem('ycloud-stay-signed-out')
  window.location.replace('/browse')
}

async function detectExistingAccess(): Promise<void> {
  if (sessionStorage.getItem('ycloud-stay-signed-out') === '1') return

  try {
    const identity = await getIdentity()
    if (identity.logged_in) {
      openBrowser()
      return
    }
    if (identity.web_password_required === false) {
      await enterGate('')
      openBrowser()
    }
  } catch {
    // Keep the login form usable while the service recovers.
  }
}

async function submit(): Promise<void> {
  if (submitting.value) return

  submitting.value = true
  errorMessage.value = ''
  try {
    await enterGate(password.value)
    openBrowser()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '暂时无法连接服务'
    submitting.value = false
    await nextTick()
    passwordInput.value?.select()
  }
}

onMounted(detectExistingAccess)
</script>

<template>
  <main class="login-shell">
    <section class="login-panel glass" aria-labelledby="login-title">
      <CloudIcon class="login-logo" />
      <h1 id="login-title">Ycloud</h1>
      <p>输入网页访问密码进入文件空间</p>
      <form @submit.prevent="submit">
        <label class="visually-hidden" for="web-password">网页访问密码</label>
        <input
          id="web-password"
          ref="passwordInput"
          v-model="password"
          class="input"
          type="password"
          autocomplete="current-password"
          autofocus
          required
        >
        <button class="btn btn-primary" type="submit" :disabled="submitting">
          {{ submitting ? '验证中…' : '进入' }}
        </button>
      </form>
      <div class="login-error" role="alert" aria-live="polite">{{ errorMessage }}</div>
    </section>
  </main>
</template>

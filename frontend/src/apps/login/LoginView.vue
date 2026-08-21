<script setup lang="ts">
import { nextTick, onMounted, ref } from 'vue'
import CloudIcon from '../../shared/components/icons/CloudIcon.vue'
import { enterGate, getIdentity } from '../../shared/api/auth'
import { useLocale } from '../../shared/i18n'
import { appPath } from '../../shared/routes'

const locale = useLocale()
const password = ref('')
const errorMessage = ref('')
const submitting = ref(false)
const passwordInput = ref<HTMLInputElement>()

function openBrowser(): void {
  sessionStorage.removeItem('ycloud-stay-signed-out')
  window.location.replace(appPath('/browse'))
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
    errorMessage.value = error instanceof Error ? error.message : locale.t('login.unavailable')
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
      <p>{{ locale.t('login.description') }}</p>
      <form @submit.prevent="submit">
        <label class="visually-hidden" for="web-password">{{ locale.t('login.webPassword') }}</label>
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
          {{ locale.t(submitting ? 'login.submitting' : 'login.enter') }}
        </button>
      </form>
      <div class="login-error" role="alert" aria-live="polite">{{ errorMessage }}</div>
    </section>
  </main>
</template>

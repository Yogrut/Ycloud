<script setup lang="ts">
import { nextTick, onMounted, ref } from 'vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { enterGate, getIdentity } from '../../shared/api/auth'
import { useLocale } from '../../shared/i18n'
import { appPath } from '../../shared/routes'

const locale = useLocale()
const password = ref('')
const errorMessage = ref('')
const submitting = ref(false)
const formRef = ref<HTMLFormElement>()

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
    formRef.value?.querySelector('input')?.select()
  }
}

onMounted(detectExistingAccess)
</script>

<template>
  <main class="login-shell">
    <div class="login-content">
      <section class="login-intro" aria-labelledby="login-intro-title">
        <span class="login-eyebrow">YCLOUD / PRIVATE FILE SPACE</span>
        <h1 id="login-intro-title">{{ locale.t('login.heroTitle') }}</h1>
        <p>{{ locale.t('login.heroDescription') }}</p>
        <span class="login-intro-rule" aria-hidden="true" />
      </section>

      <Card class="login-card w-full max-w-[500px] gap-0 rounded-xl py-0 shadow-sm">
        <CardHeader class="gap-2 px-8 pt-9 pb-7 sm:px-10 sm:pt-10">
          <CardTitle id="login-title" class="text-[25px] font-semibold tracking-tight">{{ locale.t('login.title') }}</CardTitle>
          <CardDescription class="text-sm leading-6">{{ locale.t('login.description') }}</CardDescription>
        </CardHeader>
        <CardContent class="px-8 pb-9 sm:px-10 sm:pb-10">
          <form ref="formRef" class="login-form" @submit.prevent="submit">
            <div class="login-field">
              <Label for="web-password">{{ locale.t('login.webPassword') }}</Label>
              <Input
                id="web-password"
                v-model="password"
                class="h-11 rounded-full px-4 text-sm md:text-sm"
                type="password"
                autocomplete="current-password"
                autofocus
                required
                :placeholder="locale.t('login.passwordPlaceholder')"
              />
            </div>
            <Button class="h-11 w-full rounded-full text-sm font-semibold" type="submit" :disabled="submitting">
              {{ locale.t(submitting ? 'login.submitting' : 'login.enter') }}
            </Button>
          </form>
        </CardContent>
      </Card>
      <AppFeedback :message="errorMessage" />
    </div>
  </main>
</template>

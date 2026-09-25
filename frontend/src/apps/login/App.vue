<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted } from 'vue'
import AdminView from '../admin/AdminView.vue'
import BrowserView from '../browser/BrowserView.vue'
import LoginView from './LoginView.vue'
import PreviewView from '../preview/PreviewView.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useTheme } from '../../shared/composables/useTheme'
import { currentAppPath } from '../../shared/routes'

const theme = useTheme()
const route = computed(() => currentAppPath())
const isBrowser = computed(() => route.value === '/browse')
const isAdmin = computed(() => route.value === '/admin' || route.value.startsWith('/admin/'))
const isPreview = computed(() => route.value === '/preview')

function suppressNativeContextMenu(event: MouseEvent): void {
  event.preventDefault()
}

onMounted(() => document.addEventListener('contextmenu', suppressNativeContextMenu))
onBeforeUnmount(() => document.removeEventListener('contextmenu', suppressNativeContextMenu))
</script>

<template>
  <AdminView v-if="isAdmin" :theme="theme" />
  <PreviewView v-else-if="isPreview" :theme="theme" />
  <BrowserView v-else-if="isBrowser" :theme="theme" />
  <template v-else>
    <div class="login-page">
      <header class="login-header">
        <div class="login-header-brand"><AppIcon name="cloud" :size="28" /><span>Ycloud</span></div>
        <div class="login-header-actions">
          <LocaleToggle />
          <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
        </div>
      </header>
      <LoginView />
    </div>
  </template>
</template>

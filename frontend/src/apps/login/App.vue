<script setup lang="ts">
import { computed } from 'vue'
import AdminView from '../admin/AdminView.vue'
import BrowserView from '../browser/BrowserView.vue'
import LoginView from './LoginView.vue'
import PreviewView from '../preview/PreviewView.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import { useTheme } from '../../shared/composables/useTheme'
import { currentAppPath } from '../../shared/routes'

const theme = useTheme()
const route = computed(() => currentAppPath())
const isBrowser = computed(() => route.value === '/browse')
const isAdmin = computed(() => route.value === '/admin' || route.value.startsWith('/admin/'))
const isPreview = computed(() => route.value === '/preview')
</script>

<template>
  <AdminView v-if="isAdmin" :theme="theme" />
  <PreviewView v-else-if="isPreview" :theme="theme" />
  <BrowserView v-else-if="isBrowser" :theme="theme" />
  <template v-else>
    <div class="login-floating-actions">
      <LocaleToggle />
      <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
    </div>
    <LoginView />
  </template>
</template>

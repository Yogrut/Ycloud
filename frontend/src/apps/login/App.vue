<script setup lang="ts">
import { computed } from 'vue'
import BrowserView from '../browser/BrowserView.vue'
import LoginView from './LoginView.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import { useTheme } from '../../shared/composables/useTheme'

const theme = useTheme()
const route = computed(() => window.location.pathname.replace(/\/+$/, ''))
const isBrowser = computed(() => route.value === '/v2/browse')
</script>

<template>
  <BrowserView v-if="isBrowser" :theme="theme" />
  <template v-else>
    <ThemeToggle :theme="theme.current.value" class="theme-floating" @toggle="theme.toggle" />
    <LoginView />
  </template>
</template>

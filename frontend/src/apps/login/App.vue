<script setup lang="ts">
import LoginView from './LoginView.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import { useTheme } from '../../shared/composables/useTheme'

const theme = useTheme()
</script>

<template>
  <ThemeToggle :theme="theme.current.value" class="theme-floating" @toggle="theme.toggle" />
  <LoginView />
</template>

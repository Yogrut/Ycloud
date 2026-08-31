<script setup lang="ts">
import type { Theme } from '../composables/useTheme'
import { useLocale } from '../i18n'
import AppIcon from './AppIcon.vue'

defineProps<{ theme: Theme }>()
defineEmits<{ toggle: [] }>()
const locale = useLocale()
</script>

<template>
  <button
    class="icon-btn"
    type="button"
    :title="locale.t(theme === 'dark' ? 'theme.switchToLight' : 'theme.switchToDark')"
    :aria-label="locale.t(theme === 'dark' ? 'theme.switchToLight' : 'theme.switchToDark')"
    @click="$emit('toggle')"
  >
    <AppIcon :name="theme === 'dark' ? 'moon' : 'sun'" />
  </button>
</template>

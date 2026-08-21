<script setup lang="ts">
import type { Theme } from '../composables/useTheme'
import { useLocale } from '../i18n'

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
    <svg v-if="theme === 'dark'" class="ui-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401" />
    </svg>
    <svg v-else class="ui-icon theme-sun" viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
    </svg>
  </button>
</template>

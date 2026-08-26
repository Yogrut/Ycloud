<script setup lang="ts">
import { useLocale } from '../i18n'
import AppIcon from './AppIcon.vue'

const locale = useLocale()
</script>

<template>
  <button
    class="icon-btn locale-toggle"
    type="button"
    :title="locale.t(locale.isEnglish.value ? 'locale.switchToChinese' : 'locale.switchToEnglish')"
    :aria-label="locale.t(locale.isEnglish.value ? 'locale.switchToChinese' : 'locale.switchToEnglish')"
    @click="locale.toggle"
  >
    <AppIcon name="language" />
  </button>
</template>

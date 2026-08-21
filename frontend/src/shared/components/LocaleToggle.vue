<script setup lang="ts">
import { useLocale } from '../i18n'

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
    <span aria-hidden="true">{{ locale.isEnglish.value ? '中' : 'EN' }}</span>
  </button>
</template>

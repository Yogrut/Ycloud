<script setup lang="ts">
import AppIcon from './AppIcon.vue'
import { useLocale } from '../i18n'
defineProps<{ label: string; value: string | number; description?: string }>()
defineEmits<{ edit: [] }>()
const locale = useLocale()
</script>
<template>
  <div class="setting-row">
    <span class="setting-row-label">{{ label }}</span>
    <div class="setting-row-detail">
      <div class="setting-row-control"><span class="setting-row-value">{{ value }}</span><button type="button" :aria-label="`${locale.text('设置', 'Configure')} ${label}`" @click="$emit('edit')"><AppIcon name="settings" :size="15" />{{ locale.text('设置', 'Configure') }}</button></div>
      <p v-if="description">{{ description }}</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import AppIcon, { type AppIconName } from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ entry: FileEntry }>()
const locale = useLocale()
const knownKinds = new Set(['image', 'video', 'audio', 'archive', 'pdf', 'code', 'doc'])
const kind = computed(() => props.entry.is_dir ? 'folder' : (knownKinds.has(props.entry.icon) ? props.entry.icon : 'file'))
const icon = computed<AppIconName>(() => {
  if (kind.value === 'folder' || kind.value === 'file') return kind.value
  return `file-${kind.value}` as AppIconName
})
</script>

<template>
  <span class="file-icon" :class="kind" aria-hidden="true">
    <AppIcon :name="icon" />
    <span v-if="entry.locked" class="lock-dot" :title="locale.t('file.lockedFolder')"><AppIcon name="lock" :size="10" /></span>
  </span>
</template>

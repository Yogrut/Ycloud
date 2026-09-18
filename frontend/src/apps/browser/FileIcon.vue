<script setup lang="ts">
import { computed } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import AppIcon, { type AppIconName } from '../../shared/components/AppIcon.vue'

const props = defineProps<{ entry: FileEntry }>()
const knownKinds = new Set(['image', 'video', 'audio', 'archive', 'pdf', 'code', 'doc'])
const kind = computed(() => props.entry.is_dir ? 'folder' : (knownKinds.has(props.entry.icon) ? props.entry.icon : 'file'))
const icon = computed<AppIconName>(() => {
  if (kind.value === 'folder') return 'folder'
  if (kind.value === 'file') return 'file-text'
  return `file-${kind.value}` as AppIconName
})
</script>

<template>
  <span class="file-icon" :class="kind" aria-hidden="true">
    <AppIcon :name="icon" weight="fill" />
  </span>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import AppIcon, { type AppIconName } from '../../shared/components/AppIcon.vue'

const props = defineProps<{ entry: FileEntry }>()
const knownKinds = new Set(['image', 'video', 'audio', 'archive', 'pdf', 'code', 'doc'])
const extensionIcons: Record<string, AppIconName> = {
  csv: 'file-csv',
  doc: 'file-doc',
  docx: 'file-doc',
  md: 'file-md',
  ppt: 'file-ppt',
  pptx: 'file-ppt',
  txt: 'file-text',
  xls: 'file-xls',
  xlsx: 'file-xls',
}
const kind = computed(() => props.entry.is_dir ? 'folder' : (knownKinds.has(props.entry.icon) ? props.entry.icon : 'file'))
const tone = computed(() => {
  if (kind.value === 'folder') return 'tone-folder'
  if (kind.value === 'image' || kind.value === 'video') return 'tone-visual'
  if (kind.value === 'audio') return 'tone-audio'
  if (kind.value === 'archive' || kind.value === 'file') return 'tone-utility'
  return 'tone-document'
})
const icon = computed<AppIconName>(() => {
  if (kind.value === 'folder') return 'folder-simple'
  const extension = props.entry.name.split('.').pop()?.toLowerCase() ?? ''
  if (extensionIcons[extension]) return extensionIcons[extension]
  if (kind.value === 'file') return 'file'
  return `file-${kind.value}` as AppIconName
})
</script>

<template>
  <span class="file-icon" :class="[kind, tone]" aria-hidden="true">
    <AppIcon :name="icon" :weight="kind === 'folder' ? 'fill' : 'duotone'" />
  </span>
</template>

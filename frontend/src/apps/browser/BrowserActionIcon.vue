<script setup lang="ts">
import { computed } from 'vue'
import AppIcon, { type AppIconName } from '../../shared/components/AppIcon.vue'

export type BrowserAction = 'open' | 'download' | 'archive' | 'rename' | 'move' | 'copy' | 'delete' | 'upload' | 'mkdir' | 'folder'

const props = defineProps<{ name: BrowserAction }>()
const names: Record<BrowserAction, AppIconName> = {
  open: 'folder-open', download: 'download', archive: 'archive', rename: 'rename', move: 'move',
  copy: 'copy', delete: 'delete', upload: 'upload', mkdir: 'folder-plus', folder: 'folder',
}
const icon = computed(() => names[props.name])
</script>

<template>
  <AppIcon :name="icon" />
</template>

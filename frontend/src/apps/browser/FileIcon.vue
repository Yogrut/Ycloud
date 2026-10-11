<script setup lang="ts">
import { computed } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { fileIconName, materialFileIcons } from './fileIcons'

const props = defineProps<{ entry: FileEntry }>()
const icon = computed(() => fileIconName(props.entry))
</script>

<template>
  <span class="file-icon" :class="{ folder: entry.is_dir }" aria-hidden="true">
    <img :src="materialFileIcons[icon]" :data-file-icon="icon" alt="" draggable="false">
  </span>
</template>

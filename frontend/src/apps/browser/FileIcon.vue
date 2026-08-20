<script setup lang="ts">
import { computed } from 'vue'
import type { FileEntry } from '../../shared/api/browser'

const props = defineProps<{ entry: FileEntry }>()
const knownKinds = new Set(['image', 'video', 'audio', 'archive', 'pdf', 'code', 'doc'])
const kind = computed(() => props.entry.is_dir ? 'folder' : (knownKinds.has(props.entry.icon) ? props.entry.icon : 'file'))
</script>

<template>
  <span class="file-icon" :class="kind" aria-hidden="true">
    <svg v-if="kind === 'folder'" viewBox="0 0 24 24"><path d="M3 6.5h7l2 2h9v10H3z" fill="currentColor" opacity=".22" /><path d="M3 6.5h7l2 2h9v10H3z" /></svg>
    <svg v-else-if="kind === 'image'" viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="3" /><circle cx="9" cy="9" r="2" fill="currentColor" /><path d="m5 17 4-4 3 3 2-2 5 4" /></svg>
    <svg v-else-if="kind === 'video'" viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="3" /><path d="m10 9 5 3-5 3z" fill="currentColor" /></svg>
    <svg v-else-if="kind === 'audio'" viewBox="0 0 24 24"><path d="M9 17V6l10-2v11" /><circle cx="6" cy="18" r="3" /><circle cx="16" cy="16" r="3" /></svg>
    <svg v-else-if="kind === 'archive'" viewBox="0 0 24 24"><path d="M5 3h14v18H5z" /><path d="M10 3h4v3h-4zm0 6h4v3h-4zm0 6h4v3h-4z" fill="currentColor" /></svg>
    <svg v-else-if="kind === 'code'" viewBox="0 0 24 24"><path d="M8 8 4 12l4 4m8-8 4 4-4 4m-2-10-4 12" /></svg>
    <svg v-else viewBox="0 0 24 24"><path d="M6 2h8l4 4v16H6z" /><path d="M14 2v5h5" /></svg>
    <span v-if="entry.locked" class="lock-dot" title="文件夹已加锁">●</span>
  </span>
</template>

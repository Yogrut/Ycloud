<script setup lang="ts">
export type BrowserAction = 'open' | 'download' | 'archive' | 'rename' | 'move' | 'copy' | 'delete' | 'upload' | 'mkdir' | 'folder'

defineProps<{ name: BrowserAction }>()
</script>

<template>
  <svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true">
    <template v-if="name === 'open'">
      <path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />
    </template>
    <template v-else-if="name === 'download'">
      <path d="M12 15V3M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4m4-5 5 5 5-5" />
    </template>
    <template v-else-if="name === 'archive'">
      <path d="M5 3h14v18H5z" /><path class="action-icon-fill" d="M10 3h4v3h-4zm0 6h4v3h-4zm0 6h4v3h-4z" />
    </template>
    <template v-else-if="name === 'rename'">
      <path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497zM15 5l4 4" />
    </template>
    <template v-else-if="name === 'move'">
      <path d="M12 2v20m3-3-3 3-3-3m10-10 3 3-3 3M2 12h20M5 9l-3 3 3 3M9 5l3-3 3 3" />
    </template>
    <template v-else-if="name === 'copy'">
      <rect width="14" height="14" x="8" y="8" rx="2" /><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
    </template>
    <template v-else-if="name === 'delete'">
      <path d="M10 11v6m4-6v6m5-11v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
    </template>
    <template v-else-if="name === 'upload'">
      <path d="M12 13v8M4 14.899A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.5 8.242M8 17l4-4 4 4" />
    </template>
    <template v-else-if="name === 'mkdir'">
      <path d="M12 10v6M9 13h6M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </template>
    <template v-else>
      <path d="M3 6.5h7l2 2h9v10H3z" />
    </template>
  </svg>
</template>

<style scoped>
.action-icon-fill {
  fill: currentColor;
  stroke: none;
}
</style>

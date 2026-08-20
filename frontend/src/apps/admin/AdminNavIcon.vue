<script setup lang="ts">
defineProps<{ name: 'webdav' | 'locks' | 'limits' | 'account' | 'security' }>()
</script>

<template>
  <svg v-if="name === 'webdav'" class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16v10H4zM8 4v3m8-3v3M8 17v3m8-3v3" /></svg>
  <svg v-else-if="name === 'locks'" class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="10" width="16" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></svg>
  <svg v-else-if="name === 'limits'" class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h10M18 7h2M4 17h2M10 17h10" /><circle cx="16" cy="7" r="2" /><circle cx="8" cy="17" r="2" /></svg>
  <svg v-else-if="name === 'account'" class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="4" /><path d="M4 21a8 8 0 0 1 16 0" /></svg>
  <svg v-else class="ui-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3 4.5 6v5.5c0 4.6 3.1 7.8 7.5 9.5 4.4-1.7 7.5-4.9 7.5-9.5V6L12 3Z" /><path d="m9 12 2 2 4-4" /></svg>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import AppIcon, { type AppIconName } from '../../shared/components/AppIcon.vue'

type AdminNavName = 'dashboard' | 'storage' | 'webdav' | 'locks' | 'limits' | 'account' | 'users' | 'protection' | 'security'
const props = defineProps<{ name: AdminNavName }>()
const names: Record<AdminNavName, AppIconName> = {
  dashboard: 'home', storage: 'storage', webdav: 'webdav', locks: 'lock', limits: 'limits',
  account: 'account', users: 'users', protection: 'administrator', security: 'logs',
}
const icon = computed(() => names[props.name])
</script>

<template>
  <AppIcon :name="icon" />
</template>

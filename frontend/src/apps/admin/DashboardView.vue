<script setup lang="ts">
import { computed } from 'vue'
import type { AdminInfo } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import TrafficPanel from './TrafficPanel.vue'

const props = defineProps<{ info: AdminInfo }>()
const locale = useLocale()
const metrics = computed(() => [
  { label: locale.text('存储空间', 'Storage spaces'), value: props.info.storage_instances.length },
  { label: locale.text('普通用户', 'Users'), value: props.info.user_accounts?.length ?? 0 },
  { label: locale.text('WebDAV 挂载', 'WebDAV mounts'), value: props.info.shares.length },
  { label: locale.text('文件夹锁', 'Folder locks'), value: props.info.folder_locks.length },
])
</script>

<template>
  <div class="dashboard-view" aria-labelledby="dashboard-title">
    <h1 id="dashboard-title" class="visually-hidden">{{ locale.text('仪表盘', 'Dashboard') }}</h1>
    <section class="dashboard-section glass overview-section">
      <header><h2>{{ locale.text('概览', 'Overview') }}</h2></header>
      <div class="overview-grid">
        <article v-for="metric in metrics" :key="metric.label"><span>{{ metric.label }}</span><strong>{{ metric.value }}</strong></article>
      </div>
    </section>
    <TrafficPanel mode="dashboard" />
  </div>
</template>

<style scoped>
.dashboard-view { min-width: 0; display: grid; gap: 10px; }
.dashboard-section { min-width: 0; padding: 22px 24px; border-radius: 10px; }
.dashboard-section > header h2 { margin: 0; font-size: 17px; }
.dashboard-section > header h2::before,:deep(.traffic-usage-heading h2)::before,:deep(.history-heading h2)::before { content: ''; display: inline-block; width: 4px; height: 18px; margin-right: 10px; vertical-align: -3px; background: var(--accent); border-radius: 3px; }
.overview-grid { display: grid; grid-template-columns: repeat(4,minmax(0,1fr)); gap: 12px; margin-top: 22px; }
.overview-grid article { min-width: 0; display: grid; justify-items: center; gap: 10px; padding: 10px; }
.overview-grid span { color: var(--muted); font-size: 13px; }
.overview-grid strong { color: var(--accent); font-size: 22px; font-weight: 500; }
@media(max-width: 820px) { .overview-grid { grid-template-columns: repeat(2,minmax(0,1fr)); } .dashboard-section { padding: 20px 16px; } }
</style>

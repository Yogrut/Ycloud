<script setup lang="ts">
import { useLocale } from '../i18n'

defineOptions({ inheritAttrs: false })
defineProps<{ error?: Error }>()
const locale = useLocale()

function reloadPage(): void {
  window.location.reload()
}
</script>

<template>
  <div class="page-load-state glass" :role="error ? 'alert' : 'status'" aria-live="polite">
    <template v-if="error">
      <p>{{ locale.text('页面资源加载失败，请检查网络。重新加载会离开当前页面，请先确认没有未完成操作。', 'Page resources could not load. Check your connection. Reloading leaves this page; check for unfinished operations first.') }}</p>
      <button type="button" class="btn secondary" @click="reloadPage">{{ locale.text('重新加载页面', 'Reload page') }}</button>
    </template>
    <p v-else>{{ locale.t('common.loading') }}</p>
  </div>
</template>

<style scoped>
.page-load-state { display: grid; gap: 12px; margin: 16px; padding: 24px; }
.page-load-state p { margin: 0; }
.page-load-state button { justify-self: start; }
</style>

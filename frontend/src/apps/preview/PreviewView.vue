<script setup lang="ts">
import { computed, onMounted, ref, watchEffect } from 'vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()

const requestPath = new URLSearchParams(window.location.search).get('path') ?? ''
const cleanPath = requestPath.replace(/^\/+/, '')
const name = cleanPath.split('/').filter(Boolean).pop() ?? ''
const extension = name.includes('.') ? name.split('.').pop()?.toLowerCase() ?? '' : ''
const previewUrl = `/api/preview?path=${encodeURIComponent(`/${cleanPath}`)}`
const downloadUrl = `/api/download?path=${encodeURIComponent(`/${cleanPath}`)}`
const text = ref('')
const textError = ref('')
const textTruncated = ref(false)

const kind = computed(() => {
  if (new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'avif']).has(extension)) return 'image'
  if (new Set(['mp4', 'webm', 'mov']).has(extension)) return 'video'
  if (new Set(['mp3', 'wav', 'ogg', 'flac', 'aac', 'm4a']).has(extension)) return 'audio'
  if (extension === 'pdf') return 'pdf'
  if (new Set(['txt', 'md', 'markdown', 'rs', 'py', 'js', 'ts', 'go', 'java', 'c', 'cpp', 'h', 'html', 'css', 'json', 'xml', 'yaml', 'yml', 'toml', 'sh', 'sql', 'vue', 'svelte', 'rb', 'php', 'swift', 'kt', 'cs', 'lua', 'log', 'csv']).has(extension)) return 'text'
  return 'unsupported'
})

async function loadText(): Promise<void> {
  if (kind.value !== 'text') return
  try {
    const response = await fetch(previewUrl, { credentials: 'same-origin', headers: { Range: 'bytes=0-2097151' } })
    if (!response.ok) throw new Error(`${locale.t('preview.failed')} (${response.status})`)
    text.value = await response.text()
    const contentRange = response.headers.get('Content-Range')
    const range = contentRange?.match(/^bytes\s+\d+-(\d+)\/(\d+)$/i)
    textTruncated.value = range ? Number(range[1]) + 1 < Number(range[2]) : false
  } catch (error) {
    textError.value = error instanceof Error ? error.message : locale.t('preview.failed')
  }
}

function closePreview(): void {
  window.close()
}

onMounted(() => {
  void loadText()
})

watchEffect(() => {
  const title = locale.t('preview.title')
  document.title = name ? `${name} - ${title}` : title
})
</script>

<template>
  <div class="preview-page">
    <header class="preview-toolbar">
      <span class="preview-title">{{ name || locale.t('preview.title') }}</span>
      <div class="preview-actions">
        <LocaleToggle />
        <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
        <a class="btn secondary" :href="downloadUrl">{{ locale.t('preview.download') }}</a>
        <button class="btn secondary" type="button" @click="closePreview">{{ locale.t('preview.close') }}</button>
      </div>
    </header>
    <main class="preview-content">
      <div v-if="!name" class="preview-message"><strong>{{ locale.t('preview.missingPath') }}</strong><p>{{ locale.t('preview.missingPathHelp') }}</p></div>
      <img v-else-if="kind === 'image'" :src="previewUrl" :alt="name">
      <video v-else-if="kind === 'video'" :src="previewUrl" controls />
      <audio v-else-if="kind === 'audio'" :src="previewUrl" controls />
      <iframe v-else-if="kind === 'pdf'" :src="previewUrl" :title="name" />
      <div v-else-if="kind === 'text' && textError" class="preview-message"><strong>{{ name }}</strong><p>{{ textError }}</p></div>
      <pre v-else-if="kind === 'text'">{{ text }}{{ textTruncated ? `\n\n${locale.t('preview.truncated')}` : '' }}</pre>
      <div v-else class="preview-message"><strong>{{ name }}</strong><p>{{ locale.t('preview.unsupported') }}</p><a class="btn" :href="downloadUrl">{{ locale.t('preview.downloadFile') }}</a></div>
    </main>
  </div>
</template>

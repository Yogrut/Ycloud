<script setup lang="ts">
import { computed, onMounted, ref, watchEffect } from 'vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import { checkDownload, isPreviewTrafficExhausted } from '../../shared/api/browser'
import { previewKind } from '../../shared/previewFormats'
import MediaPlayer from '../../shared/components/MediaPlayer.vue'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()

const requestPath = new URLSearchParams(window.location.search).get('path') ?? ''
const storageId = new URLSearchParams(window.location.search).get('storage_id') ?? ''
const cleanPath = requestPath.replace(/^\/+/, '')
const name = cleanPath.split('/').filter(Boolean).pop() ?? ''
const storageQuery = storageId ? `&storage_id=${encodeURIComponent(storageId)}` : ''
const previewUrl = `/api/preview?path=${encodeURIComponent(`/${cleanPath}`)}${storageQuery}`
const downloadUrl = `/api/download?path=${encodeURIComponent(`/${cleanPath}`)}${storageQuery}`
const text = ref('')
const textError = ref('')
const textTruncated = ref(false)
const mediaError = ref(false)
const trafficExhausted = ref(false)
const pdfReady = ref(false)
const pdfViewerAvailable = navigator.pdfViewerEnabled !== false

const kind = computed(() => previewKind(name))

async function loadText(): Promise<void> {
  if (kind.value !== 'text') return
  try {
    const response = await fetch(previewUrl, { credentials: 'same-origin', headers: { Range: 'bytes=0-2097151' } })
    if (response.status === 429) {
      trafficExhausted.value = true
      textError.value = locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.')
      return
    }
    if (!response.ok) throw new Error(`${locale.t('preview.failed')} (${response.status})`)
    text.value = await response.text()
    const contentRange = response.headers.get('Content-Range')
    const range = contentRange?.match(/^bytes\s+\d+-(\d+)\/(\d+)$/i)
    textTruncated.value = range ? Number(range[1]) + 1 < Number(range[2]) : false
  } catch (error) {
    textError.value = error instanceof Error ? error.message : locale.t('preview.failed')
  }
}

async function handleMediaError(): Promise<void> {
  mediaError.value = true
  trafficExhausted.value = await isPreviewTrafficExhausted(previewUrl)
}

async function checkPdfPreview(): Promise<void> {
  if (kind.value !== 'pdf' || !pdfViewerAvailable) return
  trafficExhausted.value = await isPreviewTrafficExhausted(previewUrl)
  if (trafficExhausted.value) mediaError.value = true
  else pdfReady.value = true
}

async function startDownload(): Promise<void> {
  try {
    await checkDownload(downloadUrl)
    window.location.href = downloadUrl
  } catch (error) {
    textError.value = error instanceof Error ? error.message : locale.t('preview.failed')
  }
}

function closePreview(): void {
  window.close()
}

onMounted(() => {
  void loadText()
  void checkPdfPreview()
})

watchEffect(() => {
  const title = locale.t('preview.title')
  document.title = name ? `${name} - ${title}` : title
})
</script>

<template>
  <div class="preview-page">
    <AppFeedback :message="textError" />
    <header class="preview-toolbar">
      <span class="preview-title">{{ name || locale.t('preview.title') }}</span>
      <div class="preview-actions">
        <LocaleToggle />
        <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
        <a v-if="!trafficExhausted" class="btn secondary" :href="downloadUrl" @click.prevent="startDownload">{{ locale.t('preview.download') }}</a>
        <button class="btn secondary" type="button" @click="closePreview">{{ locale.t('preview.close') }}</button>
      </div>
    </header>
    <main class="preview-content">
      <div v-if="!name" class="preview-message"><strong>{{ locale.t('preview.missingPath') }}</strong><p>{{ locale.t('preview.missingPathHelp') }}</p></div>
      <img v-else-if="kind === 'image' && !mediaError" :src="previewUrl" :alt="name" @error="handleMediaError">
      <MediaPlayer v-else-if="(kind === 'video' || kind === 'audio') && !mediaError" :src="previewUrl" :name="name" :kind="kind" @error="handleMediaError" />
      <iframe v-else-if="kind === 'pdf' && pdfViewerAvailable && !mediaError && pdfReady" :src="previewUrl" :title="name" @error="handleMediaError" />
      <div v-else-if="kind === 'pdf' && pdfViewerAvailable && !mediaError" class="preview-message">{{ locale.text('正在检查预览…', 'Checking preview…') }}</div>
      <div v-else-if="kind === 'text' && textError" class="preview-message"><strong>{{ name }}</strong><p>{{ textError }}</p><a v-if="!trafficExhausted" class="btn" :href="downloadUrl" @click.prevent="startDownload">{{ locale.t('preview.downloadFile') }}</a></div>
      <pre v-else-if="kind === 'text'">{{ text }}{{ textTruncated ? `\n\n${locale.t('preview.truncated')}` : '' }}</pre>
      <div v-else class="preview-message"><strong>{{ name }}</strong><p>{{ trafficExhausted ? locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.') : locale.t('preview.unsupported') }}</p><a v-if="!trafficExhausted" class="btn" :href="downloadUrl" @click.prevent="startDownload">{{ locale.t('preview.downloadFile') }}</a></div>
    </main>
  </div>
</template>

<script setup lang="ts">
import { computed, watchEffect } from 'vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'
import { useLocale } from '../../shared/i18n'
import { downloadUrl as fileDownloadUrl, previewUrl as filePreviewUrl } from '../../shared/api/browser'
import { previewKind } from '../../shared/previewFormats'
import MediaPlayer from '../../shared/components/MediaPlayer.vue'
import { useTextPreview } from '../../shared/composables/useTextPreview'
import { useMediaPreview } from '../../shared/composables/useMediaPreview'
import { usePreviewDownload } from '../../shared/composables/usePreviewDownload'

defineProps<{ theme: ThemeController }>()
const locale = useLocale()

const parameters = new URLSearchParams(window.location.search)
const requestPath = parameters.get('path') ?? ''
const storageId = parameters.get('storage_id') ?? ''
const cleanPath = requestPath.replace(/^\/+/, '')
const name = cleanPath.split('/').filter(Boolean).pop() ?? ''
const previewUrl = filePreviewUrl(cleanPath, storageId)
const downloadUrl = fileDownloadUrl(cleanPath, storageId)
const pdfViewerAvailable = navigator.pdfViewerEnabled !== false

const kind = computed(() => previewKind(name))
const { failed: mediaError, trafficExhausted: mediaTrafficExhausted, ready: pdfReady, onError: handleMediaError } = useMediaPreview(
  computed(() => ['image', 'video', 'audio', 'pdf'].includes(kind.value) && (kind.value !== 'pdf' || pdfViewerAvailable) ? previewUrl : ''),
  computed(() => kind.value === 'pdf'),
)
const { error: downloadError, startDownload } = usePreviewDownload(computed(() => name ? downloadUrl : ''))
const { text, error: textError, truncated: textTruncated, trafficExhausted: textTrafficExhausted } = useTextPreview(computed(() => kind.value === 'text' ? previewUrl : ''))
const trafficExhausted = computed(() => mediaTrafficExhausted.value || textTrafficExhausted.value)

function closePreview(): void {
  window.close()
}

watchEffect(() => {
  const title = locale.t('preview.title')
  document.title = name ? `${name} - ${title}` : title
})
</script>

<template>
  <div class="preview-page">
    <AppFeedback :message="downloadError || textError" />
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

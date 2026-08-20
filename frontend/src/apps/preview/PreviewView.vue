<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import type { ThemeController } from '../../shared/composables/useTheme'

defineProps<{ theme: ThemeController }>()

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
    if (!response.ok) throw new Error(`预览失败 (${response.status})`)
    text.value = await response.text()
    const contentRange = response.headers.get('Content-Range')
    const range = contentRange?.match(/^bytes\s+\d+-(\d+)\/(\d+)$/i)
    textTruncated.value = range ? Number(range[1]) + 1 < Number(range[2]) : false
  } catch (error) {
    textError.value = error instanceof Error ? error.message : '预览失败'
  }
}

function closePreview(): void {
  window.close()
}

onMounted(() => {
  document.title = name ? `${name} - 文件预览` : '文件预览'
  void loadText()
})
</script>

<template>
  <div class="preview-page">
    <header class="preview-toolbar">
      <span class="preview-title">{{ name || '文件预览' }}</span>
      <div class="preview-actions">
        <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
        <a class="btn secondary" :href="downloadUrl">下载</a>
        <button class="btn secondary" type="button" @click="closePreview">关闭</button>
      </div>
    </header>
    <main class="preview-content">
      <div v-if="!name" class="preview-message"><strong>缺少文件路径</strong><p>请返回文件浏览器后重新打开预览。</p></div>
      <img v-else-if="kind === 'image'" :src="previewUrl" :alt="name">
      <video v-else-if="kind === 'video'" :src="previewUrl" controls />
      <audio v-else-if="kind === 'audio'" :src="previewUrl" controls />
      <iframe v-else-if="kind === 'pdf'" :src="previewUrl" :title="name" />
      <div v-else-if="kind === 'text' && textError" class="preview-message"><strong>{{ name }}</strong><p>{{ textError }}</p></div>
      <pre v-else-if="kind === 'text'">{{ text }}{{ textTruncated ? '\n\n[预览已截断，仅显示前 2 MiB]' : '' }}</pre>
      <div v-else class="preview-message"><strong>{{ name }}</strong><p>此文件类型不在安全预览列表中。</p><a class="btn" :href="downloadUrl">下载文件</a></div>
    </main>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { PhArrowsOut, PhDownloadSimple, PhX } from '@phosphor-icons/vue'
import type { FileEntry } from '../../shared/api/browser'
import { checkDownload, downloadUrl, isPreviewTrafficExhausted, previewUrl } from '../../shared/api/browser'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import { previewKind } from '../../shared/previewFormats'
import AppIcon from '../../shared/components/AppIcon.vue'
import MediaPlayer from '../../shared/components/MediaPlayer.vue'

const props = defineProps<{ entry: FileEntry; storageId: string }>()
const emit = defineEmits<{ close: [] }>()
const locale = useLocale()
const kind = computed(() => previewKind(props.entry.name))
const isDocument = computed(() => kind.value === 'pdf' || kind.value === 'text')
const isHtml = computed(() => /\.html?$/i.test(props.entry.name))
const source = computed(() => previewUrl(props.entry.path, props.storageId))
const target = computed(() => downloadUrl(props.entry.path, props.storageId))
const pdfViewerAvailable = navigator.pdfViewerEnabled !== false
const failed = ref(false)
const trafficExhausted = ref(false)
const pdfReady = ref(false)
const text = ref('')
const textTruncated = ref(false)
const textReady = ref(false)
const documentMode = ref<'preview' | 'source'>('preview')
const safeHtml = computed(() => `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; font-src data:;">${text.value}`)
const downloadError = ref('')
const dialogElement = ref<HTMLElement | null>(null)
const abort = new AbortController()
let previousFocus: HTMLElement | null = null
let previousOverflow = ''

async function loadText(): Promise<void> {
  if (kind.value !== 'text') return
  try {
    const response = await fetch(source.value, { credentials: 'same-origin', headers: { Range: 'bytes=0-2097151' }, signal: abort.signal })
    if (response.status === 429) {
      trafficExhausted.value = true
      failed.value = true
      return
    }
    if (!response.ok) throw new Error(`${locale.t('preview.failed')} (${response.status})`)
    text.value = await response.text()
    textReady.value = true
    const range = response.headers.get('Content-Range')?.match(/^bytes\s+\d+-(\d+)\/(\d+)$/i)
    textTruncated.value = range ? Number(range[1]) + 1 < Number(range[2]) : false
  } catch {
    if (!abort.signal.aborted) failed.value = true
  }
}

async function handlePreviewError(): Promise<void> {
  failed.value = true
  trafficExhausted.value = await isPreviewTrafficExhausted(source.value)
}

async function checkPdfPreview(): Promise<void> {
  if (kind.value !== 'pdf' || !pdfViewerAvailable) return
  if (await isPreviewTrafficExhausted(source.value)) {
    trafficExhausted.value = true
    failed.value = true
  } else pdfReady.value = true
}

async function toggleFullscreen(): Promise<void> {
  if (document.fullscreenElement) await document.exitFullscreen()
  else await dialogElement.value?.requestFullscreen?.()
}

async function startDownload(): Promise<void> {
  try {
    downloadError.value = ''
    await checkDownload(target.value)
    window.location.href = target.value
  } catch (error) {
    downloadError.value = error instanceof Error ? error.message : locale.text('下载失败', 'Download failed')
  }
}

function keydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return
  event.preventDefault()
  event.stopPropagation()
  emit('close')
}

onMounted(async () => {
  previousFocus = document.activeElement as HTMLElement | null
  previousOverflow = document.body.style.overflow
  document.body.style.overflow = 'hidden'
  void loadText()
  void checkPdfPreview()
  await nextTick()
  dialogElement.value?.focus({ preventScroll: true })
})
onBeforeUnmount(() => {
  abort.abort()
  document.body.style.overflow = previousOverflow
  previousFocus?.focus()
})
</script>

<template>
  <Teleport to="body">
    <div class="ycloud-preview-overlay" @click.self="emit('close')" @keydown="keydown">
      <section ref="dialogElement" class="ycloud-preview-dialog" :class="{ document: isDocument, video: kind === 'video', audio: kind === 'audio' }" role="dialog" aria-modal="true" :aria-label="entry.name" tabindex="-1">
        <header class="ycloud-preview-head">
          <AppIcon v-if="!isDocument" :name="kind === 'video' ? 'file-video' : 'file-audio'" :size="30" />
          <div class="ycloud-preview-heading"><strong>{{ entry.name }}</strong><span v-if="!isDocument">{{ formatSize(entry.size) }} · {{ entry.modified }}</span></div>
          <div v-if="isHtml && textReady && !textTruncated" class="ycloud-document-modes" :aria-label="locale.text('文档视图', 'Document view')">
            <button type="button" :class="{ active: documentMode === 'preview' }" @click="documentMode = 'preview'">{{ locale.text('预览', 'Preview') }}</button>
            <button type="button" :class="{ active: documentMode === 'source' }" @click="documentMode = 'source'">{{ locale.text('源码', 'Source') }}</button>
          </div>
          <button v-if="isDocument" class="ycloud-preview-icon" type="button" :aria-label="locale.text('全屏', 'Fullscreen')" @click="toggleFullscreen"><PhArrowsOut :size="20" /></button>
          <a v-if="!trafficExhausted" class="ycloud-preview-icon" :href="target" :title="locale.t('preview.download')" :aria-label="locale.t('preview.download')" @click.prevent="startDownload"><PhDownloadSimple :size="21" /></a>
          <button class="ycloud-preview-icon ycloud-preview-close" type="button" :aria-label="locale.t('preview.close')" @click="emit('close')"><PhX :size="22" /></button>
        </header>
        <main class="ycloud-preview-body">
          <div v-if="failed || (kind === 'pdf' && !pdfViewerAvailable)" class="ycloud-preview-fallback">
            <strong>{{ trafficExhausted ? locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.') : locale.text('浏览器无法预览此文件', 'This file cannot be previewed in the browser') }}</strong>
            <a v-if="!trafficExhausted" class="btn" :href="target" @click.prevent="startDownload">{{ locale.t('preview.downloadFile') }}</a>
          </div>
          <MediaPlayer v-else-if="kind === 'video' || kind === 'audio'" :src="source" :name="entry.name" :kind="kind" @error="handlePreviewError" />
          <iframe v-else-if="kind === 'pdf' && pdfReady" :src="source" :title="entry.name" @error="handlePreviewError" />
          <div v-else-if="kind === 'pdf'" class="ycloud-preview-fallback">{{ locale.text('正在检查预览…', 'Checking preview…') }}</div>
          <iframe v-else-if="isHtml && textReady && !textTruncated && documentMode === 'preview'" :srcdoc="safeHtml" :title="entry.name" sandbox="" referrerpolicy="no-referrer" />
          <pre v-else-if="kind === 'text'">{{ text }}{{ textTruncated ? `\n\n${locale.t('preview.truncated')}` : '' }}</pre>
          <div v-else class="ycloud-preview-fallback"><a class="btn" :href="target" @click.prevent="startDownload">{{ locale.t('preview.downloadFile') }}</a></div>
        </main>
        <p v-if="downloadError" class="ycloud-preview-error">{{ downloadError }}</p>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.ycloud-preview-overlay { position: fixed; z-index: 240; inset: 0; display: grid; place-items: center; padding: 24px; background: rgb(6 8 12 / 72%); backdrop-filter: blur(4px); }
.ycloud-preview-dialog { width: min(1180px, 100%); max-height: calc(100vh - 48px); display: flex; flex-direction: column; overflow: hidden; color: var(--text); background: var(--panel); border: 1px solid var(--line); border-radius: 5px; box-shadow: 0 22px 75px rgb(0 0 0 / 25%); }
.ycloud-preview-dialog:focus { outline: none; }
.ycloud-preview-dialog.video, .ycloud-preview-dialog.document { width: min(1040px, 100%); height: min(630px, calc(100vh - 48px)); }
.ycloud-preview-dialog.video { border: 0; }
.ycloud-preview-head { display: flex; align-items: center; gap: 14px; min-height: 62px; padding: 10px 20px; border-bottom: 1px solid var(--line); }
.ycloud-preview-dialog.video .ycloud-preview-head { box-sizing: border-box; min-height: 0; height: 48px; flex: 0 0 48px; gap: 10px; padding: 4px 14px; border-bottom: 0; }
.ycloud-preview-dialog.video .ycloud-preview-icon { width: 32px; height: 32px; }
.ycloud-preview-head > .app-icon { flex: 0 0 auto; color: var(--accent); }
.ycloud-preview-heading { display: grid; gap: 4px; min-width: 0; flex: 1; }
.ycloud-preview-heading strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 15px; }
.ycloud-preview-heading span { color: var(--muted); font-size: 12px; }
.ycloud-preview-head .btn { min-height: 36px; display: inline-flex; align-items: center; text-decoration: none; }
.ycloud-document-modes { display: flex; gap: 2px; padding: 3px; background: var(--panel-soft); border: 1px solid var(--line); border-radius: 5px; }
.ycloud-document-modes button { padding: 6px 14px; color: var(--muted); background: transparent; border: 0; border-radius: 3px; cursor: pointer; }
.ycloud-document-modes button.active { color: var(--text); background: var(--panel); box-shadow: 0 1px 3px rgb(0 0 0 / 8%); }
.ycloud-preview-icon { width: 38px; height: 38px; flex: 0 0 auto; display: grid; place-items: center; padding: 0; color: var(--muted); background: transparent; border: 0; border-radius: 6px; text-decoration: none; cursor: pointer; }
.ycloud-preview-icon:hover { color: var(--text); background: var(--panel-soft); }
.ycloud-preview-icon svg { display: block; }
.ycloud-preview-body { min-height: 240px; display: grid; place-items: center; overflow: auto; padding: 20px; }
.ycloud-preview-dialog.video .ycloud-preview-body { min-height: 0; flex: 1; display: block; overflow: hidden; padding: 0; background: #080a0d; }
.ycloud-preview-dialog.video :deep(.ycloud-media-player) { width: 100%; height: 100%; border: 0; border-radius: 0; }
.ycloud-preview-dialog.video :deep(.ycloud-video-stage) { height: 100%; max-height: none; aspect-ratio: auto; }
.ycloud-preview-dialog.audio .ycloud-preview-body { min-height: 180px; padding: 30px; background: var(--panel); }
.ycloud-preview-dialog.document .ycloud-preview-body { min-height: 0; flex: 1; display: flex; align-items: stretch; overflow: hidden; padding: 0; background: var(--panel); }
.ycloud-preview-body iframe { width: 100%; height: min(74vh, 900px); border: 0; background: #fff; }
.ycloud-preview-dialog.document .ycloud-preview-body iframe { height: 100%; }
.ycloud-preview-body pre { width: 100%; max-height: 74vh; overflow: auto; margin: 0; color: var(--text); white-space: pre-wrap; overflow-wrap: anywhere; font: 13px/1.6 ui-monospace, SFMono-Regular, Consolas, monospace; }
.ycloud-preview-dialog.document .ycloud-preview-body pre { max-height: none; box-sizing: border-box; padding: 28px 34px; background: var(--panel); }
.ycloud-preview-fallback { display: grid; justify-items: center; gap: 18px; color: var(--muted); text-align: center; }
.ycloud-preview-fallback strong { color: var(--text); }
.ycloud-preview-fallback .btn { text-decoration: none; }
.ycloud-preview-error { margin: 0; padding: 8px 20px 16px; color: var(--danger); font-size: 13px; }
@media (max-width: 640px) { .ycloud-preview-overlay { padding: 8px; } .ycloud-preview-dialog { max-height: calc(100vh - 16px); } .ycloud-preview-dialog.video, .ycloud-preview-dialog.document { height: calc(100vh - 16px); } .ycloud-preview-head { gap: 8px; padding: 10px; } .ycloud-preview-heading span { display: none; } .ycloud-preview-body { padding: 10px; } .ycloud-preview-dialog.audio .ycloud-preview-body { padding: 12px; } .ycloud-preview-dialog.document .ycloud-preview-body pre { padding: 16px; } .ycloud-document-modes button { padding: 6px 8px; } }
</style>

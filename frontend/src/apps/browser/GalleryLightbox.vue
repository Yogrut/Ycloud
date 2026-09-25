<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { PhX } from '@phosphor-icons/vue'
import type { FileEntry } from '../../shared/api/browser'
import { checkDownload, downloadUrl, isPreviewTrafficExhausted, previewUrl } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{
  entry: FileEntry
  storageId: string
  index: number
  total: number
}>()
const emit = defineEmits<{ close: []; previous: []; next: [] }>()
const locale = useLocale()
const zoom = ref(100)
const panX = ref(0)
const panY = ref(0)
const dragging = ref(false)
const imageError = ref(false)
const trafficExhausted = ref(false)
const downloadError = ref('')
const minZoom = 50
const maxZoom = 300
const zoomStep = 25
let dragStartX = 0
let dragStartY = 0
let dragOriginX = 0
let dragOriginY = 0

function resetPan(): void {
  dragging.value = false
  panX.value = 0
  panY.value = 0
}

function setZoom(value: number): void {
  zoom.value = Math.min(maxZoom, Math.max(minZoom, value))
}

function resetZoom(): void {
  zoom.value = 100
  resetPan()
}

function startPan(event: PointerEvent): void {
  if (event.button !== 0) return
  event.preventDefault()
  dragging.value = true
  dragStartX = event.clientX
  dragStartY = event.clientY
  dragOriginX = panX.value
  dragOriginY = panY.value
  ;(event.currentTarget as HTMLElement).setPointerCapture?.(event.pointerId)
}

function movePan(event: PointerEvent): void {
  if (!dragging.value) return
  panX.value = dragOriginX + event.clientX - dragStartX
  panY.value = dragOriginY + event.clientY - dragStartY
}

function stopPan(event: PointerEvent): void {
  if (!dragging.value) return
  dragging.value = false
  ;(event.currentTarget as HTMLElement).releasePointerCapture?.(event.pointerId)
}

function handleWheel(event: WheelEvent): void {
  if (imageError.value) return
  setZoom(zoom.value + (event.deltaY < 0 ? zoomStep : -zoomStep))
}

async function startDownload(): Promise<void> {
  const url = downloadUrl(props.entry.path, props.storageId)
  try {
    downloadError.value = ''
    await checkDownload(url)
    window.location.href = url
  } catch (error) {
    downloadError.value = error instanceof Error ? error.message : locale.text('下载失败', 'Download failed')
  }
}

async function handleImageError(): Promise<void> {
  const path = props.entry.path
  imageError.value = true
  const exhausted = await isPreviewTrafficExhausted(previewUrl(path, props.storageId))
  if (props.entry.path === path) trafficExhausted.value = exhausted
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') emit('close')
  if (event.key === 'ArrowLeft' && props.index > 0) emit('previous')
  if (event.key === 'ArrowRight' && props.index < props.total - 1) emit('next')
  if (event.key === '+' || event.key === '=') setZoom(zoom.value + zoomStep)
  if (event.key === '-') setZoom(zoom.value - zoomStep)
  if (event.key === '0') resetZoom()
}

watch(() => props.entry.path, () => {
  resetZoom()
  imageError.value = false
  trafficExhausted.value = false
  downloadError.value = ''
})
onMounted(() => document.addEventListener('keydown', handleKeydown))
onBeforeUnmount(() => document.removeEventListener('keydown', handleKeydown))
</script>

<template>
  <Teleport to="body">
    <div class="gallery-lightbox" role="dialog" aria-modal="true" :aria-label="entry.name" @click.self="emit('close')">
      <div class="gallery-lightbox-meta">
        <span>{{ index + 1 }} / {{ total }}</span>
        <strong>{{ entry.name }}</strong>
      </div>
      <button class="gallery-lightbox-close" type="button" :aria-label="locale.text('关闭', 'Close')" @click="emit('close')">
        <PhX :size="23" />
      </button>
      <button class="gallery-lightbox-nav previous" type="button" :disabled="index <= 0" :aria-label="locale.text('上一张图片', 'Previous image')" @click="emit('previous')">
        <span class="gallery-nav-chevron" aria-hidden="true" />
      </button>
      <div class="gallery-lightbox-stage" @wheel.prevent="handleWheel">
        <img
          v-if="!imageError"
          :src="previewUrl(entry.path, storageId)"
          :alt="entry.name"
          :class="{ 'is-panning': dragging }"
          :style="{ transform: `translate(${panX}px, ${panY}px) scale(${zoom / 100})` }"
          draggable="false"
          @dblclick="zoom === 100 ? setZoom(200) : resetZoom()"
          @dragstart.prevent
          @pointerdown="startPan"
          @pointermove="movePan"
          @pointerup="stopPan"
          @pointercancel="stopPan"
          @error="handleImageError"
        >
        <div v-else class="gallery-lightbox-unavailable">
          <strong>{{ trafficExhausted ? locale.text('下载流量已用尽或剩余流量不足，请等待重置或联系管理员', 'Download allowance is exhausted or insufficient. Wait for the reset or contact the administrator.') : locale.text('浏览器无法预览此图片', 'This image cannot be previewed in the browser') }}</strong>
          <a v-if="!trafficExhausted" :href="downloadUrl(entry.path, storageId)" @click.prevent="startDownload">{{ locale.text('下载文件', 'Download file') }}</a>
          <p v-if="downloadError">{{ downloadError }}</p>
        </div>
      </div>
      <button class="gallery-lightbox-nav next" type="button" :disabled="index >= total - 1" :aria-label="locale.text('下一张图片', 'Next image')" @click="emit('next')">
        <span class="gallery-nav-chevron" aria-hidden="true" />
      </button>
      <div v-if="!imageError" class="gallery-lightbox-toolbar" role="toolbar" :aria-label="locale.text('图片缩放', 'Image zoom')">
        <button type="button" :disabled="zoom <= minZoom" :aria-label="locale.text('缩小图片', 'Zoom out')" @click="setZoom(zoom - zoomStep)">−</button>
        <button class="gallery-zoom-value" type="button" :aria-label="locale.text('恢复原始缩放', 'Reset zoom')" @click="resetZoom">{{ zoom }}%</button>
        <button type="button" :disabled="zoom >= maxZoom" :aria-label="locale.text('放大图片', 'Zoom in')" @click="setZoom(zoom + zoomStep)">+</button>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.gallery-lightbox {
  position: fixed;
  z-index: 260;
  inset: 0;
  color: white;
  background: rgb(6 8 12 / 74%);
  backdrop-filter: blur(3px);
}
.gallery-lightbox-meta {
  position: absolute;
  z-index: 3;
  top: 14px;
  left: 50%;
  max-width: min(70vw, 720px);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 7px;
  pointer-events: none;
  transform: translateX(-50%);
}
.gallery-lightbox-meta span {
  padding: 5px 12px;
  color: rgb(255 255 255 / 88%);
  background: rgb(18 20 24 / 72%);
  border-radius: 999px;
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  box-shadow: 0 4px 18px rgb(0 0 0 / 18%);
}
.gallery-lightbox-meta strong {
  max-width: 100%;
  overflow: hidden;
  font-size: 13px;
  text-align: center;
  text-overflow: ellipsis;
  text-shadow: 0 1px 5px rgb(0 0 0 / 90%);
  white-space: nowrap;
}
.gallery-lightbox-close {
  position: absolute;
  z-index: 4;
  top: 16px;
  right: 18px;
  width: 42px;
  height: 42px;
  display: grid;
  place-items: center;
  padding: 0;
  color: white;
  background: transparent;
  border: 0;
  border-radius: 50%;
  cursor: pointer;
}
.gallery-lightbox-close:hover { opacity: .72; }
.gallery-lightbox-close svg { filter: drop-shadow(0 1px 3px rgb(0 0 0 / 85%)); }
.gallery-lightbox-stage {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  padding: 76px 72px 74px;
  box-sizing: border-box;
}
.gallery-lightbox-stage img {
  flex: 0 1 auto;
  min-width: 0;
  min-height: 0;
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
  transform-origin: center;
  transition: transform .16s ease;
  box-shadow: 0 18px 70px rgb(0 0 0 / 36%);
  cursor: grab;
  touch-action: none;
  user-select: none;
}
.gallery-lightbox-stage img.is-panning { cursor: grabbing; transition: none; }
.gallery-lightbox-unavailable {
  display: grid;
  justify-items: center;
  gap: 14px;
  text-align: center;
}
.gallery-lightbox-unavailable a { color: white; background: var(--accent); border-radius: 6px; padding: 9px 18px; text-decoration: none; }
.gallery-lightbox-unavailable p { margin: 0; color: white; }
.gallery-lightbox-nav {
  position: absolute;
  z-index: 3;
  top: 50%;
  width: 40px;
  height: 40px;
  display: grid;
  place-items: center;
  padding: 0;
  color: white;
  background: rgb(18 20 24 / 64%);
  border: 1px solid rgb(255 255 255 / 12%);
  border-radius: 50%;
  cursor: pointer;
  transform: translateY(-50%);
}
.gallery-nav-chevron {
  width: 8px;
  height: 8px;
  display: block;
  border-top: 1.75px solid currentColor;
  border-right: 1.75px solid currentColor;
}
.gallery-lightbox-nav.next .gallery-nav-chevron { transform: rotate(45deg); }
.gallery-lightbox-nav.previous .gallery-nav-chevron { transform: rotate(-135deg); }
.gallery-lightbox-nav:hover:not(:disabled) { background: rgb(42 44 49 / 82%); }
.gallery-lightbox-nav:disabled { opacity: .2; cursor: default; }
.gallery-lightbox-nav.previous { left: 18px; }
.gallery-lightbox-nav.next { right: 18px; }
.gallery-lightbox-toolbar {
  position: absolute;
  z-index: 4;
  bottom: 18px;
  left: 50%;
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 4px;
  background: rgb(18 20 24 / 84%);
  border: 1px solid rgb(255 255 255 / 12%);
  border-radius: 9px;
  box-shadow: 0 8px 28px rgb(0 0 0 / 28%);
  transform: translateX(-50%);
}
.gallery-lightbox-toolbar button {
  min-width: 32px;
  height: 30px;
  display: grid;
  place-items: center;
  padding: 0 8px;
  color: white;
  background: transparent;
  border: 0;
  border-radius: 6px;
  font: inherit;
  font-size: 16px;
  cursor: pointer;
}
.gallery-lightbox-toolbar button:hover:not(:disabled) { background: rgb(255 255 255 / 10%); }
.gallery-lightbox-toolbar button:disabled { opacity: .35; cursor: default; }
.gallery-lightbox-toolbar .gallery-zoom-value {
  min-width: 62px;
  color: rgb(255 255 255 / 82%);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}
@media (max-width: 640px) {
  .gallery-lightbox-stage { padding: 74px 50px 72px; }
  .gallery-lightbox-nav { width: 36px; height: 36px; font-size: 25px; }
  .gallery-lightbox-nav.previous { left: 8px; }
  .gallery-lightbox-nav.next { right: 8px; }
  .gallery-lightbox-close { top: 12px; right: 12px; width: 38px; height: 38px; }
  .gallery-lightbox-meta { max-width: 62vw; }
}
@media (prefers-reduced-motion: reduce) {
  .gallery-lightbox-stage img { transition: none; }
}
</style>

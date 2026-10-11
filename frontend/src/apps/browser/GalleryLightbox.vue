<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { PhX } from '@phosphor-icons/vue'
import type { FileEntry } from '../../shared/api/browser'
import { downloadUrl, previewUrl } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { useMediaPreview } from '../../shared/composables/useMediaPreview'
import { usePreviewDownload } from '../../shared/composables/usePreviewDownload'

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
const source = computed(() => previewUrl(props.entry.path, props.storageId))
const target = computed(() => downloadUrl(props.entry.path, props.storageId))
const { failed: imageError, trafficExhausted, onError: handleImageError } = useMediaPreview(source)
const { error: downloadError, startDownload } = usePreviewDownload(target)
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

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') emit('close')
  if (event.key === 'ArrowLeft' && props.index > 0) emit('previous')
  if (event.key === 'ArrowRight' && props.index < props.total - 1) emit('next')
  if (event.key === '+' || event.key === '=') setZoom(zoom.value + zoomStep)
  if (event.key === '-') setZoom(zoom.value - zoomStep)
  if (event.key === '0') resetZoom()
}

watch(source, resetZoom, { flush: 'sync' })
onMounted(() => document.addEventListener('keydown', handleKeydown))
onBeforeUnmount(() => document.removeEventListener('keydown', handleKeydown))
</script>

<template>
  <Teleport to="body">
    <div class="gallery-lightbox" role="dialog" aria-modal="true" :aria-label="entry.name" @click.self="emit('close')">
      <section class="gallery-lightbox-panel">
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
            :src="source"
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
            <a v-if="!trafficExhausted" :href="target" @click.prevent="startDownload">{{ locale.text('下载文件', 'Download file') }}</a>
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
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.gallery-lightbox {
  position: fixed;
  z-index: 260;
  inset: 0;
  display: grid;
  place-items: center;
  color: white;
  background: rgb(6 8 12 / 74%);
  backdrop-filter: blur(3px);
}
.gallery-lightbox-panel { position: relative; width: 100%; height: 100%; color: var(--text); background: var(--panel-soft); }
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
  color: var(--muted);
  background: var(--panel);
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
  color: inherit;
  background: transparent;
  border: 0;
  border-radius: 50%;
  cursor: pointer;
}
.gallery-lightbox-close:hover { opacity: .72; }
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
.gallery-lightbox-unavailable p { margin: 0; color: inherit; }
.gallery-lightbox-nav {
  position: absolute;
  z-index: 3;
  top: 50%;
  width: 40px;
  height: 40px;
  display: grid;
  place-items: center;
  padding: 0;
  color: inherit;
  background: var(--panel);
  border: 1px solid var(--line-strong);
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
.gallery-lightbox-nav:hover:not(:disabled) { background: var(--field); }
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
  background: var(--panel);
  border: 1px solid var(--line-strong);
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
  color: inherit;
  background: transparent;
  border: 0;
  border-radius: 6px;
  font: inherit;
  font-size: 16px;
  cursor: pointer;
}
.gallery-lightbox-toolbar button:hover:not(:disabled) { background: var(--panel-soft); }
.gallery-lightbox-toolbar button:disabled { opacity: .35; cursor: default; }
.gallery-lightbox-toolbar .gallery-zoom-value {
  min-width: 62px;
  color: var(--muted);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}
@media (max-width: 1024px), (pointer: coarse) {
  .gallery-lightbox-panel { width: min(1040px, calc(100% - 32px)); height: min(720px, 78dvh); overflow: hidden; border: 1px solid var(--line-strong); border-radius: 8px; }
  .gallery-lightbox-stage { padding: 64px 42px 58px; }
  .gallery-lightbox-nav { width: 36px; height: 36px; font-size: 25px; }
  .gallery-lightbox-nav.previous { left: 8px; }
  .gallery-lightbox-nav.next { right: 8px; }
  .gallery-lightbox-close { top: 12px; right: 12px; width: 38px; height: 38px; }
  .gallery-lightbox-meta { max-width: calc(100% - 112px); }
}
@media (prefers-reduced-motion: reduce) {
  .gallery-lightbox-stage img { transition: none; }
}
</style>

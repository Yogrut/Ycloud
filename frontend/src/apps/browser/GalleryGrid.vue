<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { previewUrl } from '../../shared/api/browser'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ entries: FileEntry[]; storageId: string; selected?: ReadonlySet<string> }>()
const emit = defineEmits<{ open: [entry: FileEntry]; select: [entry: FileEntry]; contextMenu: [event: MouseEvent, entry: FileEntry] }>()
const locale = useLocale()

const supportedExtensions = new Set(['jpg', 'jpeg', 'png', 'webp', 'avif', 'gif'])
const gap = 4
const targetHeight = 190
const gallery = ref<HTMLElement | null>(null)
const galleryWidth = ref(960)
const ratios = reactive<Record<string, number>>({})
let resizeObserver: ResizeObserver | undefined

function isSupportedImage(entry: FileEntry): boolean {
  if (entry.is_dir) return false
  const extension = entry.name.includes('.') ? entry.name.split('.').pop()?.toLowerCase() ?? '' : ''
  return supportedExtensions.has(extension)
}

const images = computed(() => props.entries.filter(isSupportedImage))

function rememberRatio(path: string, event: Event): void {
  const image = event.currentTarget as HTMLImageElement
  if (image.naturalWidth && image.naturalHeight) ratios[path] = image.naturalWidth / image.naturalHeight
}

function updateWidth(): void {
  if (gallery.value?.clientWidth) galleryWidth.value = gallery.value.clientWidth
}

const rows = computed(() => {
  const result: { height: number; items: { entry: FileEntry; width: number }[] }[] = []
  let row: FileEntry[] = []
  let ratioSum = 0
  const availableWidth = galleryWidth.value

  function addRow(justify: boolean): void {
    if (!row.length) return
    const rowGap = gap * (row.length - 1)
    const height = justify ? (availableWidth - rowGap) / ratioSum : Math.min(targetHeight, (availableWidth - rowGap) / ratioSum)
    result.push({ height, items: row.map(entry => ({ entry, width: (ratios[entry.path] ?? 1.5) * height })) })
    row = []
    ratioSum = 0
  }

  for (const entry of images.value) {
    row.push(entry)
    ratioSum += ratios[entry.path] ?? 1.5
    if (ratioSum * targetHeight + gap * (row.length - 1) >= availableWidth) addRow(true)
  }
  addRow(false)
  return result
})

onMounted(() => {
  updateWidth()
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(updateWidth)
    if (gallery.value) resizeObserver.observe(gallery.value)
  } else {
    window.addEventListener('resize', updateWidth)
  }
})
onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  window.removeEventListener('resize', updateWidth)
})
</script>

<template>
  <div ref="gallery" class="gallery-grid" :aria-label="locale.text(`画廊，共 ${images.length} 张图片`, `Gallery, ${images.length} images`)">
    <div v-for="row in rows" :key="row.items[0]?.entry.path" class="gallery-row">
      <div
        v-for="item in row.items"
        :key="item.entry.path"
        class="gallery-card"
        :class="{ selected: selected?.has(item.entry.path) }"
        :style="{ width: `${item.width}px`, height: `${row.height}px` }"
        @contextmenu.stop="emit('contextMenu', $event, item.entry)"
      >
        <button class="gallery-card-open" type="button" :aria-label="item.entry.name" @mousedown.stop @click="emit('open', item.entry)">
          <img :src="previewUrl(item.entry.path, storageId)" :alt="item.entry.name" loading="lazy" decoding="async" @load="rememberRatio(item.entry.path, $event)">
          <span class="gallery-card-fallback" aria-hidden="true"><AppIcon name="file-image" :size="28" /></span>
        </button>
        <button class="gallery-select" :class="{ checked: selected?.has(item.entry.path) }" type="button" :aria-pressed="selected?.has(item.entry.path) ?? false" :aria-label="locale.text(`选择 ${item.entry.name}`, `Select ${item.entry.name}`)" @click.stop="emit('select', item.entry)" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.gallery-grid {
  width: 100%;
  flex: 0 0 auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 4px 0 18px;
  user-select: none;
}
.gallery-row { display: flex; gap: 4px; }
.gallery-card {
  position: relative;
  flex: 0 0 auto;
  overflow: hidden;
  margin: 0;
  background: var(--panel-soft);
  isolation: isolate;
}
.gallery-card.selected::after { position: absolute; z-index: 2; inset: 0; content: ''; border: 2px solid var(--accent); pointer-events: none; }
.gallery-card-open { width: 100%; height: 100%; display: block; padding: 0; background: transparent; border: 0; cursor: zoom-in; }
.gallery-card-open img {
  position: relative;
  z-index: 1;
  width: 100%;
  height: 100%;
  display: block;
  object-fit: cover;
}
.gallery-card-open:focus-visible, .gallery-select:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.gallery-select { position: absolute; z-index: 3; top: 9px; left: 9px; width: 22px; height: 22px; padding: 0; opacity: 0; pointer-events: none; background: rgb(12 20 35 / 48%); border: 1.5px solid white; border-radius: 50%; box-shadow: 0 1px 5px rgb(0 0 0 / 28%); cursor: pointer; transition: opacity .15s ease; }
.gallery-card:hover .gallery-select, .gallery-card-open:focus-visible + .gallery-select, .gallery-select:focus-visible, .gallery-select.checked { opacity: 1; pointer-events: auto; }
.gallery-select:hover { background: rgb(12 20 35 / 70%); }
.gallery-select.checked { background: var(--accent); border-color: white; }
.gallery-select.checked::after { position: absolute; top: 3px; left: 7px; width: 5px; height: 10px; content: ''; transform: rotate(45deg); border: solid white; border-width: 0 2px 2px 0; }
@media (hover: none) { .gallery-select { opacity: 1; pointer-events: auto; } }
@media (prefers-reduced-motion: reduce) { .gallery-select { transition: none; } }
.gallery-card-fallback {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  color: var(--muted-2);
}
</style>

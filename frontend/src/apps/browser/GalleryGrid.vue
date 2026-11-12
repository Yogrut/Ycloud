<script setup lang="ts">
import { computed, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'
import { previewKind } from '../../shared/previewFormats'
import { galleryImageKey, useGalleryThumbnails } from './useGalleryThumbnails'

const props = withDefaults(defineProps<{ entries: FileEntry[]; storageId: string; canDownload?: boolean; paused?: boolean; selected?: ReadonlySet<string> }>(), { canDownload: true })
const emit = defineEmits<{ open: [entry: FileEntry]; select: [entry: FileEntry]; contextMenu: [event: MouseEvent, entry: FileEntry] }>()
const locale = useLocale()

const grid = ref<HTMLElement | null>(null)
const imageCards = computed(() => props.entries
  .filter(entry => !entry.is_dir && previewKind(entry.name) === 'image')
  .map(entry => ({ entry, key: galleryImageKey(entry, props.storageId) })))
const candidates = computed(() => new Map(imageCards.value
  .filter(({ entry }) => props.canDownload && !entry.locked)
  .map(({ entry, key }) => [key, entry])))
const { sources, ratios, status } = useGalleryThumbnails(grid, candidates, () => props)
const cards = computed(() => imageCards.value.filter(({ key }) => status.get(key) !== 'invalid'))
const statusLabel = (key: string) => {
  switch (status.get(key)) {
    case 'pending': return locale.text('正在生成缩略图', 'Generating thumbnail')
    case 'traffic': return locale.text('下载流量不足', 'Download allowance insufficient')
    case 'forbidden': return locale.text('图片不可访问', 'Image not accessible')
    case 'limited': return locale.text('图片超出缩图处理范围', 'Image exceeds thumbnail limits')
    case 'unavailable': return locale.text('无法生成缩略图', 'Thumbnail unavailable')
    case 'network': return locale.text('缩略图加载失败', 'Thumbnail request failed')
    default: return ''
  }
}
defineExpose({ thumbnailFor: (entry: FileEntry) => sources.get(galleryImageKey(entry, props.storageId)) })
</script>

<template>
  <div ref="grid" class="gallery-grid" :aria-label="locale.text(`画廊，共 ${cards.length} 张图片`, `Gallery, ${cards.length} images`)">
    <div
      v-for="{ entry, key } in cards"
      :key="key"
      class="gallery-card"
      :data-gallery-key="key"
      :data-entry-path="entry.path"
      :class="{ selected: selected?.has(entry.path) }"
      @contextmenu.stop="emit('contextMenu', $event, entry)"
    >
      <button class="gallery-card-open" type="button" :aria-label="entry.name" :style="{ aspectRatio: ratios.get(key) ?? 1.5 }" @mousedown.stop @click="emit('open', entry)">
        <img v-if="candidates.has(key) && sources.has(key)" :src="sources.get(key)" :alt="entry.name" decoding="async" @load="($event.currentTarget as HTMLImageElement).classList.add('is-loaded')" @error="($event.currentTarget as HTMLImageElement).classList.remove('is-loaded')">
        <span v-if="!sources.has(key)" class="gallery-card-fallback">
          <span v-if="statusLabel(key)">{{ statusLabel(key) }}</span>
          <AppIcon v-else name="file-image" :size="28" aria-hidden="true" />
        </span>
      </button>
      <button class="gallery-select" :class="{ checked: selected?.has(entry.path) }" type="button" :aria-pressed="selected?.has(entry.path) ?? false" :aria-label="locale.text(`选择 ${entry.name}`, `Select ${entry.name}`)" @click.stop="emit('select', entry)" />
    </div>
  </div>
  <div v-if="!cards.length && imageCards.length" class="empty">{{ locale.text('没有可展示的图片', 'No images to display') }}</div>
</template>

<style scoped>
.gallery-grid {
  width: 100%;
  column-count: 3;
  column-gap: 32px;
  font-size: 0;
  user-select: none;
}
.gallery-card {
  display: inline-block;
  position: relative;
  width: 100%;
  overflow: hidden;
  break-inside: avoid;
  margin: 0 0 16px;
  vertical-align: top;
  border-radius: 6px;
  isolation: isolate;
}
.gallery-card.selected::after { position: absolute; z-index: 2; inset: 0; content: ''; border: 2px solid var(--accent); pointer-events: none; }
.gallery-card-open { position: relative; width: 100%; display: block; padding: 0; background: transparent; border: 0; line-height: 0; cursor: zoom-in; -webkit-touch-callout: none; }
.gallery-card-open img {
  position: absolute;
  inset: 0;
  z-index: 1;
  width: 100%;
  height: 100%;
  object-fit: contain;
  display: block;
  opacity: 0;
}
.gallery-card-open img.is-loaded { opacity: 1; }
.gallery-card-open:focus-visible, .gallery-select:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.gallery-select { position: absolute; z-index: 3; top: 9px; left: 9px; width: 22px; height: 22px; padding: 0; opacity: 0; pointer-events: none; background: rgb(12 20 35 / 48%); border: 1.5px solid white; border-radius: 50%; box-shadow: 0 1px 5px rgb(0 0 0 / 28%); cursor: pointer; transition: opacity .15s ease; }
.gallery-card:hover .gallery-select, .gallery-card-open:focus-visible + .gallery-select, .gallery-select:focus-visible, .gallery-select.checked { opacity: 1; pointer-events: auto; }
.gallery-select:hover { background: rgb(12 20 35 / 70%); }
.gallery-select.checked { background: var(--accent); border-color: white; }
.gallery-select.checked::after { position: absolute; top: 3px; left: 7px; width: 5px; height: 10px; content: ''; transform: rotate(45deg); border: solid white; border-width: 0 2px 2px 0; }
@media (hover: none) { .gallery-select { opacity: 1; pointer-events: auto; } }
@media (max-width: 960px) { .gallery-grid { column-count: 2; column-gap: 20px; } }
@media (max-width: 520px) { .gallery-grid { column-gap: 12px; } .gallery-card { margin-bottom: 12px; } }
@media (prefers-reduced-motion: reduce) { .gallery-select { transition: none; } }
.gallery-card-fallback {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  color: var(--muted-2);
  font-size: .85rem;
  line-height: 1.3;
}
</style>

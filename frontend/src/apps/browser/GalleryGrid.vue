<script setup lang="ts">
import { computed } from 'vue'
import { thumbnailUrl, type FileEntry } from '../../shared/api/browser'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useLocale } from '../../shared/i18n'
import { previewKind } from '../../shared/previewFormats'

const props = withDefaults(defineProps<{ entries: FileEntry[]; storageId: string; canDownload?: boolean; selected?: ReadonlySet<string> }>(), { canDownload: true })
const emit = defineEmits<{ open: [entry: FileEntry]; select: [entry: FileEntry]; contextMenu: [event: MouseEvent, entry: FileEntry] }>()
const locale = useLocale()

const imageEntries = computed(() => props.entries.filter(entry => !entry.is_dir && previewKind(entry.name) === 'image'))
</script>

<template>
  <div class="gallery-grid" :aria-label="locale.text(`画廊，共 ${imageEntries.length} 张图片`, `Gallery, ${imageEntries.length} images`)">
    <div
      v-for="entry in imageEntries"
      :key="`${storageId}:${entry.path}`"
      class="gallery-card"
      :data-entry-path="entry.path"
      :class="{ selected: selected?.has(entry.path) }"
      @contextmenu.stop="emit('contextMenu', $event, entry)"
    >
      <button class="gallery-card-open" type="button" :aria-label="entry.name" @mousedown.stop @click="emit('open', entry)">
        <img v-if="canDownload !== false && !entry.locked && !/\.avif$/i.test(entry.name)" :key="thumbnailUrl(entry, storageId)" :src="thumbnailUrl(entry, storageId)" :alt="entry.name" loading="lazy" decoding="async" fetchpriority="low" @load="($event.currentTarget as HTMLImageElement).classList.add('is-loaded')" @error="($event.currentTarget as HTMLImageElement).classList.remove('is-loaded')">
        <span class="gallery-card-fallback" aria-hidden="true"><AppIcon name="file-image" :size="28" /></span>
      </button>
      <button class="gallery-select" :class="{ checked: selected?.has(entry.path) }" type="button" :aria-pressed="selected?.has(entry.path) ?? false" :aria-label="locale.text(`选择 ${entry.name}`, `Select ${entry.name}`)" @click.stop="emit('select', entry)" />
    </div>
  </div>
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
  position: relative;
  z-index: 1;
  width: 100%;
  height: auto;
  aspect-ratio: auto 3 / 2;
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
}
.gallery-card-fallback:first-child { position: relative; aspect-ratio: 3 / 2; }
</style>

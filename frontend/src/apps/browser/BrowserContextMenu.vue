<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import BrowserActionIcon from './BrowserActionIcon.vue'
import type { BrowserAction } from './BrowserActionIcon.vue'

const props = defineProps<{
  entry: FileEntry | null
  paths: string[]
  capabilities?: BrowserCapabilities
  canWrite?: boolean
  x: number
  y: number
}>()

const emit = defineEmits<{
  action: [action: BrowserAction]
  clear: []
}>()
const locale = useLocale()

const menu = ref<HTMLElement>()
const left = ref(8)
const top = ref(8)
const count = computed(() => props.paths.length)
const multiple = computed(() => count.value > 1)
const selectionMenu = computed(() => count.value > 0)
const rights = computed<BrowserCapabilities>(() => props.capabilities ?? {
  download: true,
  upload: Boolean(props.canWrite),
  create_directory: Boolean(props.canWrite),
  rename: Boolean(props.canWrite),
  move_items: Boolean(props.canWrite),
  copy: Boolean(props.canWrite),
  delete: Boolean(props.canWrite),
})
const hasReadActions = computed(() => selectionMenu.value && (rights.value.download || (!multiple.value && props.entry?.is_dir)))
const hasEditActions = computed(() => selectionMenu.value && (
  (!multiple.value && rights.value.rename) || rights.value.move_items || rights.value.copy || rights.value.delete
))

async function place(): Promise<void> {
  left.value = props.x
  top.value = props.y
  await nextTick()
  const bounds = menu.value?.getBoundingClientRect()
  if (!bounds || window.matchMedia('(max-width: 760px)').matches) return
  left.value = Math.max(8, Math.min(props.x, window.innerWidth - bounds.width - 8))
  top.value = Math.max(8, Math.min(props.y, window.innerHeight - bounds.height - 8))
}

function choose(action: BrowserAction): void {
  emit('action', action)
}

watch(() => [props.x, props.y], place)
onMounted(place)
</script>

<template>
  <aside ref="menu" class="context-menu active" :style="{ left: `${left}px`, top: `${top}px` }" role="menu" :aria-label="locale.t('menu.fileActions')" @click.stop>
    <div v-if="selectionMenu" class="menu-caption" :class="{ 'mobile-only': count === 1 }">
      <span>{{ locale.t('menu.selected', { count }) }}</span>
      <button class="menu-clear" type="button" @click="$emit('clear')">{{ locale.t('menu.clear') }}</button>
    </div>
    <div class="menu-actions">
      <template v-if="multiple">
        <button v-if="rights.download" class="menu-item" type="button" role="menuitem" @click="choose('archive')"><BrowserActionIcon name="archive" /><span>{{ locale.t('menu.archive') }}</span></button>
        <template v-if="rights.move_items || rights.copy || rights.delete">
          <span v-if="hasReadActions" class="menu-separator" aria-hidden="true" />
          <button v-if="rights.move_items" class="menu-item" type="button" role="menuitem" @click="choose('move')"><BrowserActionIcon name="move" /><span>{{ locale.t('menu.move') }}</span></button>
          <button v-if="rights.copy" class="menu-item" type="button" role="menuitem" @click="choose('copy')"><BrowserActionIcon name="copy" /><span>{{ locale.t('menu.copy') }}</span></button>
          <button v-if="rights.delete" class="menu-item danger" type="button" role="menuitem" @click="choose('delete')"><BrowserActionIcon name="delete" /><span>{{ locale.t('menu.deleteCount', { count }) }}</span></button>
        </template>
      </template>
      <template v-else-if="selectionMenu">
        <button v-if="entry?.is_dir" class="menu-item" type="button" role="menuitem" @click="choose('open')"><BrowserActionIcon name="open" /><span>{{ locale.t('menu.open') }}</span></button>
        <button v-if="entry?.is_dir && rights.download" class="menu-item" type="button" role="menuitem" @click="choose('archive')"><BrowserActionIcon name="archive" /><span>{{ locale.t('menu.archive') }}</span></button>
        <button v-else-if="rights.download" class="menu-item" type="button" role="menuitem" @click="choose('download')"><BrowserActionIcon name="download" /><span>{{ locale.t('menu.download') }}</span></button>
        <template v-if="rights.rename || rights.move_items || rights.copy || rights.delete">
          <span v-if="hasReadActions" class="menu-separator" aria-hidden="true" />
          <button v-if="rights.rename" class="menu-item" type="button" role="menuitem" @click="choose('rename')"><BrowserActionIcon name="rename" /><span>{{ locale.t('menu.rename') }}</span></button>
          <button v-if="rights.move_items" class="menu-item" type="button" role="menuitem" @click="choose('move')"><BrowserActionIcon name="move" /><span>{{ locale.t('menu.move') }}</span></button>
          <button v-if="rights.copy" class="menu-item" type="button" role="menuitem" @click="choose('copy')"><BrowserActionIcon name="copy" /><span>{{ locale.t('menu.copy') }}</span></button>
          <button v-if="rights.delete" class="menu-item danger" type="button" role="menuitem" @click="choose('delete')"><BrowserActionIcon name="delete" /><span>{{ locale.t('common.delete') }}</span></button>
        </template>
      </template>
      <span v-if="hasReadActions || hasEditActions" class="menu-separator" aria-hidden="true" />
      <button class="menu-item" type="button" role="menuitem" @click="choose('upload')"><BrowserActionIcon name="upload" /><span>{{ locale.t('menu.upload') }}</span></button>
      <button class="menu-item" type="button" role="menuitem" @click="choose('mkdir')"><BrowserActionIcon name="mkdir" /><span>{{ locale.t('menu.newFolder') }}</span></button>
    </div>
  </aside>
</template>

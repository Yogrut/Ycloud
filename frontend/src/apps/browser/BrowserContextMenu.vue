<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import BrowserActionIcon from './BrowserActionIcon.vue'
import type { BrowserAction } from './BrowserActionIcon.vue'

const props = defineProps<{
  entry: FileEntry | null
  paths: string[]
  canWrite: boolean
  x: number
  y: number
}>()

const emit = defineEmits<{
  action: [action: BrowserAction]
  clear: []
}>()

const menu = ref<HTMLElement>()
const left = ref(8)
const top = ref(8)
const count = computed(() => props.paths.length)
const multiple = computed(() => count.value > 1)
const selectionMenu = computed(() => count.value > 0)

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
  <aside ref="menu" class="context-menu active" :style="{ left: `${left}px`, top: `${top}px` }" role="menu" aria-label="文件操作" @click.stop>
    <div v-if="selectionMenu" class="menu-caption" :class="{ 'mobile-only': count === 1 }">
      <span>已选择 {{ count }} 项</span>
      <button class="menu-clear" type="button" @click="$emit('clear')">清除</button>
    </div>
    <div class="menu-actions">
      <template v-if="!selectionMenu">
        <button v-if="canWrite" class="menu-item" type="button" role="menuitem" @click="choose('upload')"><BrowserActionIcon name="upload" /><span>上传文件</span></button>
        <button v-if="canWrite" class="menu-item" type="button" role="menuitem" @click="choose('mkdir')"><BrowserActionIcon name="mkdir" /><span>新建文件夹</span></button>
      </template>
      <template v-else-if="multiple">
        <button class="menu-item" type="button" role="menuitem" @click="choose('archive')"><BrowserActionIcon name="archive" /><span>打包下载</span></button>
        <template v-if="canWrite">
          <span class="menu-separator" aria-hidden="true" />
          <button class="menu-item" type="button" role="menuitem" @click="choose('move')"><BrowserActionIcon name="move" /><span>移动</span></button>
          <button class="menu-item" type="button" role="menuitem" @click="choose('copy')"><BrowserActionIcon name="copy" /><span>复制</span></button>
          <span class="menu-separator" aria-hidden="true" />
          <button class="menu-item danger" type="button" role="menuitem" @click="choose('delete')"><BrowserActionIcon name="delete" /><span>删除 ({{ count }})</span></button>
        </template>
      </template>
      <template v-else>
        <button v-if="entry?.is_dir" class="menu-item" type="button" role="menuitem" @click="choose('open')"><BrowserActionIcon name="open" /><span>打开</span></button>
        <button v-if="entry?.is_dir" class="menu-item" type="button" role="menuitem" @click="choose('archive')"><BrowserActionIcon name="archive" /><span>打包下载</span></button>
        <button v-else class="menu-item" type="button" role="menuitem" @click="choose('download')"><BrowserActionIcon name="download" /><span>下载</span></button>
        <template v-if="canWrite">
          <span class="menu-separator" aria-hidden="true" />
          <button class="menu-item" type="button" role="menuitem" @click="choose('rename')"><BrowserActionIcon name="rename" /><span>重命名</span></button>
          <button class="menu-item" type="button" role="menuitem" @click="choose('move')"><BrowserActionIcon name="move" /><span>移动</span></button>
          <button class="menu-item" type="button" role="menuitem" @click="choose('copy')"><BrowserActionIcon name="copy" /><span>复制</span></button>
          <span class="menu-separator" aria-hidden="true" />
          <button class="menu-item danger" type="button" role="menuitem" @click="choose('delete')"><BrowserActionIcon name="delete" /><span>删除</span></button>
        </template>
      </template>
    </div>
  </aside>
</template>

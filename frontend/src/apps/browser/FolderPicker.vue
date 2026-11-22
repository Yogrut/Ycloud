<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { createFolder, listFiles } from '../../shared/api/browser'
import { ApiError } from '../../shared/api/client'
import { useLocale } from '../../shared/i18n'
import FileIcon from './FileIcon.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import AppIcon from '../../shared/components/AppIcon.vue'

const props = defineProps<{ title: string; storageId: string }>()
const emit = defineEmits<{ close: []; confirm: [path: string, conflict: 'keep_both' | 'overwrite'] }>()
const locale = useLocale()

const path = ref('')
const directories = ref<FileEntry[]>([])
const loading = ref(false)
const error = ref('')
const canCreate = ref(false)
const creating = ref(false)
const newName = ref('')
const createError = ref('')
const creatingBusy = ref(false)
const nameInput = ref<HTMLInputElement>()
let requestVersion = 0

const segments = computed(() => {
  const parts = path.value ? path.value.split('/') : []
  return parts.map((name, index) => ({ name, path: parts.slice(0, index + 1).join('/') }))
})

async function load(destination: string): Promise<void> {
  const version = ++requestVersion
  loading.value = true
  error.value = ''
  path.value = destination.replace(/^\/+|\/+$/g, '')
  try {
    let cursor: string | undefined
    let first = true
    // The first page is enough to start choosing; remaining pages append while
    // the dialog stays usable instead of holding it in a loading state.
    do {
      const data = await listFiles(destination, props.storageId, { limit: 100, cursor })
      if (version !== requestVersion) return
      path.value = data.current_path.replace(/^\/+|\/+$/g, '')
      const folders = data.entries.filter(entry => entry.is_dir && !entry.locked)
      if (first) {
        directories.value = folders
        canCreate.value = data.capabilities?.create_directory !== false
        first = false
        loading.value = false
      } else {
        directories.value.push(...folders)
      }
      cursor = data.next_cursor ?? undefined
    } while (cursor)
  } catch (reason) {
    if (version === requestVersion) error.value = reason instanceof Error ? reason.message : locale.t('picker.loadFailed')
  } finally {
    if (version === requestVersion) loading.value = false
  }
}

function choose(conflict: 'keep_both' | 'overwrite'): void {
  if (loading.value || error.value) return
  emit('confirm', path.value, conflict)
}

function openCreate(): void {
  creating.value = true
  newName.value = ''
  createError.value = ''
  void nextTick(() => nameInput.value?.focus())
}

function cancelCreate(): void {
  creating.value = false
  newName.value = ''
  createError.value = ''
}

async function submitCreate(): Promise<void> {
  const name = newName.value.trim()
  if (!name || creatingBusy.value) return
  // Remember where the request was made: the user may navigate while it runs.
  const target = path.value
  creatingBusy.value = true
  createError.value = ''
  try {
    await createFolder(target, name, props.storageId)
    creating.value = false
    newName.value = ''
    // Refresh only the directory that is still on screen.
    if (path.value === target) await load(target)
  } catch (reason) {
    if (reason instanceof ApiError && (reason.status === 409 || reason.code === 'conflict')) {
      createError.value = locale.text('该名称已存在', 'That name already exists')
    } else {
      createError.value = reason instanceof Error
        ? reason.message
        : locale.text('创建文件夹失败', 'Could not create the folder')
    }
  } finally {
    creatingBusy.value = false
  }
}

onMounted(() => load(''))
onBeforeUnmount(() => { requestVersion++ })
</script>

<template>
  <div class="overlay active" @click.self="emit('close')">
    <section class="modal picker-modal" aria-labelledby="picker-title">
      <h2 id="picker-title">{{ title }}</h2>
      <nav class="breadcrumb picker-crumbs" :aria-label="locale.t('picker.path')">
        <button class="crumb home-crumb" type="button" :aria-label="locale.text('首页', 'Home')" :title="locale.text('首页', 'Home')" @click="load('')"><AppIcon name="home" /></button>
        <template v-for="segment in segments" :key="segment.path">
          <span class="crumb-separator" aria-hidden="true">›</span>
          <button class="crumb" type="button" :aria-current="segment.path === path ? 'location' : undefined" :title="segment.name" @click="load(segment.path)">{{ segment.name }}</button>
        </template>
        <button v-if="canCreate" class="crumb picker-new-folder" type="button" :aria-label="locale.text('新建文件夹', 'New folder')" :title="locale.text('新建文件夹', 'New folder')" @click="openCreate"><AppIcon name="folder-plus" /></button>
      </nav>
      <form v-if="creating" class="picker-create" @submit.prevent="submitCreate">
        <input
          ref="nameInput"
          v-model="newName"
          class="input"
          :placeholder="locale.text('文件夹名称', 'Folder name')"
          :disabled="creatingBusy"
          @keydown.esc.prevent="cancelCreate"
        >
        <button class="btn secondary" type="button" :disabled="creatingBusy" @click="cancelCreate">{{ locale.t('common.cancel') }}</button>
        <button class="btn" type="submit" :disabled="creatingBusy || !newName.trim()">{{ locale.text('创建', 'Create') }}</button>
        <p v-if="createError" class="picker-create-error">{{ createError }}</p>
      </form>
      <div class="picker-list">
        <button v-for="directory in directories" :key="directory.path" class="picker-row" type="button" :disabled="loading" @click="load(directory.path)">
          <FileIcon :entry="directory" />
          <span>{{ directory.name }}</span>
        </button>
        <p v-if="loading && !directories.length" class="picker-state">{{ locale.t('common.loading') }}</p>
        <p v-else-if="!error && !directories.length && path" class="picker-state">{{ locale.t('picker.emptyFolder') }}</p>
        <p v-else-if="!error && !directories.length" class="picker-state">{{ locale.t('picker.emptyRoot') }}</p>
        <AppFeedback :message="error" />
      </div>
      <div class="modal-actions picker-actions">
        <button class="btn secondary" type="button" @click="emit('close')">{{ locale.t('common.cancel') }}</button>
        <div class="picker-choices">
          <button class="btn" type="button" :disabled="loading || !!error" @click="choose('keep_both')">{{ locale.t('picker.keepBoth') }}</button>
          <button class="btn" type="button" :disabled="loading || !!error" @click="choose('overwrite')">{{ locale.t('picker.overwrite') }}</button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
/* The card keeps one size: the folder list scrolls inside it and every row is
   the same height, so switching folders never moves the frame. */
.picker-modal {
  display: flex;
  flex-direction: column;
  width: min(560px, calc(100vw - 32px));
  height: min(620px, calc(100vh - 96px));
  border-radius: var(--dialog-radius);
}
.picker-modal h2 { margin: 0 0 12px; }
.picker-crumbs { flex: 0 0 auto; margin-bottom: 10px; }
.picker-crumbs .crumb { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
.picker-new-folder { margin-left: auto; flex: 0 0 auto; }
.picker-create { display: flex; flex: 0 0 auto; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 10px; }
.picker-create .input { flex: 1 1 160px; min-width: 0; height: 36px; }
/* Same button shape as the action row; only the height follows the input. */
.picker-create .btn { min-width: 60px; min-height: 36px; height: 36px; padding: 6px 14px; font-size: 14px; font-weight: 400; line-height: 20px; }
.picker-create-error { flex: 1 0 100%; margin: 0; color: var(--danger); font-size: 13px; }
.picker-list { flex: 1; min-height: 0; max-height: none; overflow-y: auto; }
.picker-row { min-height: 46px; }
.picker-state { display: grid; place-items: center; min-height: 100%; margin: 0; }
.picker-actions { flex: 0 0 auto; justify-content: space-between; }
.picker-choices { display: flex; gap: 8px; }
.picker-actions .btn { min-width: 60px; min-height: 32px; height: 32px; padding: 5px 14px; font-size: 14px; font-weight: 400; line-height: 20px; }
</style>

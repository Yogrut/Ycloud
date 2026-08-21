<script setup lang="ts">
import { onMounted, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { listFiles } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import BrowserActionIcon from './BrowserActionIcon.vue'

defineProps<{ title: string }>()
const emit = defineEmits<{ close: []; confirm: [path: string] }>()
const locale = useLocale()

const path = ref('')
const directories = ref<FileEntry[]>([])
const loading = ref(false)
const error = ref('')

async function load(destination: string): Promise<void> {
  loading.value = true
  error.value = ''
  try {
    const data = await listFiles(destination)
    path.value = data.current_path.replace(/^\/+|\/+$/g, '')
    directories.value = data.entries.filter(entry => entry.is_dir && !entry.locked)
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : locale.t('picker.loadFailed')
  } finally {
    loading.value = false
  }
}

function parentPath(): string {
  return path.value.split('/').slice(0, -1).join('/')
}

onMounted(() => load(''))
</script>

<template>
  <div class="overlay active" @click.self="emit('close')">
    <section class="modal picker-modal" aria-labelledby="picker-title">
      <h2 id="picker-title">{{ title }}</h2>
      <div class="picker-path">{{ path ? `/${path}` : locale.t('picker.root') }}</div>
      <div class="picker-list">
        <button v-if="path" class="picker-row" type="button" @click="load(parentPath())">{{ locale.t('picker.parent') }}</button>
        <button v-for="directory in directories" :key="directory.path" class="picker-row" type="button" @click="load(directory.path)">
          <BrowserActionIcon name="folder" />
          <span>{{ directory.name }}</span>
        </button>
        <p v-if="loading" class="picker-state">{{ locale.t('common.loading') }}</p>
        <p v-else-if="error" class="modal-error">{{ error }}</p>
        <p v-else-if="!directories.length && !path" class="picker-state">{{ locale.t('picker.emptyRoot') }}</p>
      </div>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="emit('close')">{{ locale.t('common.cancel') }}</button><button class="btn" type="button" :disabled="loading || !!error" @click="emit('confirm', path)">{{ locale.t('picker.choose') }}</button></div>
    </section>
  </div>
</template>

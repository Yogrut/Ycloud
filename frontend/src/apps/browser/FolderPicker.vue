<script setup lang="ts">
import { onMounted, ref } from 'vue'
import type { FileEntry } from '../../shared/api/browser'
import { listFiles } from '../../shared/api/browser'
import BrowserActionIcon from './BrowserActionIcon.vue'

defineProps<{ title: string }>()
const emit = defineEmits<{ close: []; confirm: [path: string] }>()

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
    error.value = reason instanceof Error ? reason.message : '目录加载失败'
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
      <div class="picker-path">{{ path ? `/${path}` : '/ 根目录' }}</div>
      <div class="picker-list">
        <button v-if="path" class="picker-row" type="button" @click="load(parentPath())">↩ 返回上级</button>
        <button v-for="directory in directories" :key="directory.path" class="picker-row" type="button" @click="load(directory.path)">
          <BrowserActionIcon name="folder" />
          <span>{{ directory.name }}</span>
        </button>
        <p v-if="loading" class="picker-state">正在加载…</p>
        <p v-else-if="error" class="modal-error">{{ error }}</p>
        <p v-else-if="!directories.length && !path" class="picker-state">根目录下没有文件夹</p>
      </div>
      <div class="modal-actions"><button class="btn secondary" type="button" @click="emit('close')">取消</button><button class="btn" type="button" :disabled="loading || !!error" @click="emit('confirm', path)">选择此目录</button></div>
    </section>
  </div>
</template>

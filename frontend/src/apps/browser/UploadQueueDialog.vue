<script setup lang="ts">
import { computed, ref } from 'vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import { formatSize } from '../../shared/format'
import { useLocale } from '../../shared/i18n'
import type { UploadTask, UploadTaskStatus } from './uploadQueue'

type UploadFilter = 'all' | 'active' | 'succeeded' | 'failed' | 'cancelled'

const props = defineProps<{
  tasks: UploadTask[]
}>()

const emit = defineEmits<{
  chooseFiles: []
  chooseFolder: []
  close: []
  pause: [ids: number[]]
  resume: [ids: number[]]
  terminate: [ids: number[]]
  clear: [ids: number[]]
  retry: [id: number]
  removeFailed: [id: number]
  drop: [event: DragEvent]
}>()

const locale = useLocale()
const filter = ref<UploadFilter>('all')
const dropActive = ref(false)
let dragDepth = 0

const succeeded = computed(() => countStatus('succeeded'))
const failed = computed(() => countStatus('failed'))
const cancelled = computed(() => countStatus('cancelled'))
const active = computed(() => props.tasks.filter(task => ['preparing', 'queued', 'uploading', 'paused', 'verifying'].includes(task.status)).length)
const visibleTasks = computed(() => props.tasks.filter(task => {
  if (filter.value === 'all') return true
  if (filter.value === 'active') return task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading' || task.status === 'paused' || task.status === 'verifying'
  return task.status === filter.value
}))
const visibleTaskIds = computed(() => visibleTasks.value.map(task => task.id))
const visibleHasPausableTasks = computed(() => visibleTasks.value.some(task => task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading'))
const visibleHasPausedTasks = computed(() => visibleTasks.value.some(task => task.status === 'paused'))
const visibleHasActiveTasks = computed(() => visibleTasks.value.some(task => task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading' || task.status === 'paused' || task.status === 'verifying'))
const modalHeight = computed(() => Math.min(650, 390 + Math.min(visibleTasks.value.length, 5) * 52))
const totalBytes = computed(() => props.tasks.reduce((total, task) => total + task.file.size, 0))
const uploadedBytes = computed(() => props.tasks.reduce((total, task) => {
  if (task.status === 'succeeded') return total + task.file.size
  if (task.status !== 'uploading' && task.status !== 'verifying') return total
  return total + Math.min(task.file.size, task.loaded)
}, 0))
const overallPercent = computed(() => totalBytes.value
  ? Math.min(succeeded.value === props.tasks.length ? 100 : 99, Math.round(uploadedBytes.value / totalBytes.value * 100))
  : props.tasks.length && succeeded.value === props.tasks.length ? 100 : 0)
const completionLabel = computed(() => {
  if (!props.tasks.length) return locale.text('尚无上传任务', 'No uploads yet')
  if (succeeded.value === props.tasks.length) return locale.text('全部上传成功', 'All uploads succeeded')
  if (active.value) return locale.text(`已完成 ${succeeded.value} / ${props.tasks.length} 个文件`, `${succeeded.value} / ${props.tasks.length} files complete`)
  return locale.text(`已完成 ${succeeded.value} / ${props.tasks.length} 个文件，${failed.value + cancelled.value} 个未成功`, `${succeeded.value} / ${props.tasks.length} files complete, ${failed.value + cancelled.value} unsuccessful`)
})

const filters = computed<Array<{ value: UploadFilter; label: string; count: number }>>(() => [
  { value: 'all', label: locale.text('全部', 'All'), count: props.tasks.length },
  { value: 'active', label: locale.text('进行中', 'Active'), count: active.value },
  { value: 'succeeded', label: locale.text('成功', 'Succeeded'), count: succeeded.value },
  { value: 'failed', label: locale.text('失败/异常', 'Failed'), count: failed.value },
  { value: 'cancelled', label: locale.text('已终止', 'Terminated'), count: cancelled.value },
])

function countStatus(status: UploadTaskStatus): number {
  return props.tasks.filter(task => task.status === status).length
}

function taskStatus(task: UploadTask): string {
  switch (task.status) {
    case 'preparing': return locale.text('准备上传', 'Preparing')
    case 'queued': return locale.text('等待上传', 'Queued')
    case 'uploading': return locale.text('正在上传', 'Uploading')
    case 'paused': return locale.text('已暂停', 'Paused')
    case 'verifying': return locale.text('结果确认中', 'Verifying')
    case 'succeeded': return locale.text('成功', 'Succeeded')
    case 'failed': return locale.text('失败/异常', 'Failed')
    case 'cancelled': return locale.text('已终止', 'Terminated')
  }
}

function taskPercent(task: UploadTask): number {
  if (!task.file.size) return task.status === 'succeeded' ? 100 : 0
  if (task.status === 'succeeded') return 100
  if (task.status !== 'uploading' && task.status !== 'verifying') return 0
  return Math.min(99, Math.round(task.loaded / task.file.size * 100))
}

function statusClass(task: UploadTask): Record<string, boolean> {
  return {
    danger: task.status === 'failed',
    warning: task.status === 'paused' || task.status === 'verifying',
    muted: task.status === 'preparing' || task.status === 'queued' || task.status === 'cancelled',
  }
}

function handleDragEnter(event: DragEvent): void {
  if (!Array.from(event.dataTransfer?.types ?? []).includes('Files')) return
  dragDepth += 1
  dropActive.value = true
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy'
}

function handleDragOver(event: DragEvent): void {
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy'
}

function handleDragLeave(): void {
  dragDepth = Math.max(0, dragDepth - 1)
  if (!dragDepth) dropActive.value = false
}

function handleDrop(event: DragEvent): void {
  dragDepth = 0
  dropActive.value = false
  emit('drop', event)
}
</script>

<template>
  <div class="overlay active upload-window-overlay" @click.self="emit('close')">
    <section class="modal upload-queue-modal" :style="{ '--upload-modal-height': `${modalHeight}px` }" role="dialog" aria-modal="true" aria-labelledby="upload-title">
      <header class="upload-window-head">
        <div><h2 id="upload-title">{{ locale.text('上传', 'Upload') }}</h2><p>{{ locale.text('查看每个文件的上传状态和异常原因', 'Review each file status and error') }}</p></div>
        <button class="icon-btn flat upload-window-close" type="button" :title="locale.t('common.close')" :aria-label="locale.t('common.close')" @click="emit('close')">×</button>
      </header>

      <div class="upload-add-zone" :class="{ active: dropActive }" @dragenter.stop.prevent="handleDragEnter" @dragover.stop.prevent="handleDragOver" @dragleave.stop.prevent="handleDragLeave" @drop.stop.prevent="handleDrop">
        <div class="upload-add-copy"><AppIcon name="upload" :size="22" /><span>{{ locale.text('拖拽文件或文件夹到此处，或点击右侧选择', 'Drop files or folders here, or choose on the right') }}</span></div>
        <div class="upload-add-actions">
          <button class="btn secondary compact" type="button" @click="emit('chooseFiles')">{{ locale.text('选择文件', 'Choose files') }}</button>
          <button class="btn secondary compact folder" type="button" @click="emit('chooseFolder')">{{ locale.text('选择文件夹', 'Choose folder') }}</button>
        </div>
      </div>

      <div class="upload-queue-toolbar">
        <div class="upload-filter-tabs" role="tablist" :aria-label="locale.text('上传状态筛选', 'Upload status filter')">
          <button v-for="item in filters" :key="item.value" class="upload-filter-tab" :class="{ active: filter === item.value }" type="button" role="tab" :aria-selected="filter === item.value" @click="filter = item.value">{{ item.label }} <span>{{ item.count }}</span></button>
        </div>
        <div class="upload-batch-actions" :aria-label="locale.text('当前筛选任务控制', 'Filtered upload controls')">
          <span class="upload-total-progress">{{ locale.text('总进度', 'Total') }}</span>
          <button class="record-icon-btn" type="button" :disabled="!visibleHasPausableTasks" :title="locale.text('暂停当前筛选中的未完成任务', 'Pause unfinished tasks in this filter')" :aria-label="locale.text('暂停当前筛选任务', 'Pause filtered tasks')" @click="emit('pause', visibleTaskIds)"><AppIcon name="pause" :size="17" /></button>
          <button class="record-icon-btn" type="button" :disabled="!visibleHasPausedTasks" :title="locale.text('继续当前筛选中的任务', 'Resume filtered tasks')" :aria-label="locale.text('继续当前筛选任务', 'Resume filtered tasks')" @click="emit('resume', visibleTaskIds)"><AppIcon name="resume" :size="17" /></button>
          <button class="record-icon-btn" type="button" :disabled="!visibleHasActiveTasks" :title="locale.text('终止当前筛选中的未完成任务', 'Terminate unfinished filtered tasks')" :aria-label="locale.text('终止当前筛选任务', 'Terminate filtered tasks')" @click="emit('terminate', visibleTaskIds)"><AppIcon name="stop" :size="17" /></button>
          <button class="record-icon-btn danger" type="button" :disabled="!visibleTasks.length || visibleHasActiveTasks" :title="locale.text('删除当前筛选中的任务记录，不会删除已上传文件', 'Delete filtered task records without deleting uploaded files')" :aria-label="locale.text('删除当前筛选任务记录', 'Delete filtered task records')" @click="emit('clear', visibleTaskIds)"><AppIcon name="delete" :size="17" /></button>
        </div>
      </div>

      <progress class="upload-overall-progress" :value="overallPercent" max="100" :aria-label="locale.text('全部文件上传进度', 'Overall upload progress')">{{ overallPercent }}%</progress>
      <div class="upload-overall-meta"><span><strong>{{ completionLabel }}</strong> · {{ locale.text(`成功 ${succeeded} · 失败/异常 ${failed} · 已终止 ${cancelled}`, `${succeeded} succeeded · ${failed} failed · ${cancelled} terminated`) }}</span><span class="upload-overall-percent">{{ overallPercent }}%</span></div>

      <div v-if="visibleTasks.length" class="upload-task-list">
        <article v-for="task in visibleTasks" :key="task.id" class="upload-task" :class="`is-${task.status}`">
          <span class="upload-task-icon"><AppIcon name="file" :size="22" /></span>
          <div class="upload-task-content">
            <div class="upload-task-title"><strong :title="task.relativePath">{{ task.relativePath }}</strong><span class="status-pill" :class="statusClass(task)">{{ taskStatus(task) }}</span></div>
            <div class="upload-task-meta"><span>{{ formatSize(task.file.size) }}</span><span>{{ taskPercent(task) }}%</span></div>
            <progress :value="taskPercent(task)" max="100" :aria-label="locale.text(`${task.relativePath} 上传进度`, `${task.relativePath} upload progress`)">{{ taskPercent(task) }}%</progress>
            <p v-if="task.error" class="upload-task-error">{{ task.error }}</p>
          </div>
          <div class="upload-task-actions">
            <button v-if="task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading'" class="record-icon-btn" type="button" :title="locale.text('暂停该文件', 'Pause this file')" :aria-label="locale.text('暂停该文件', 'Pause this file')" @click="emit('pause', [task.id])"><AppIcon name="pause" :size="16" /></button>
            <button v-if="task.status === 'paused'" class="record-icon-btn" type="button" :title="locale.text('继续该文件', 'Resume this file')" :aria-label="locale.text('继续该文件', 'Resume this file')" @click="emit('resume', [task.id])"><AppIcon name="resume" :size="16" /></button>
            <button v-if="task.status === 'preparing' || task.status === 'queued' || task.status === 'uploading' || task.status === 'paused'" class="record-icon-btn" type="button" :title="locale.text('终止该文件', 'Terminate this file')" :aria-label="locale.text('终止该文件', 'Terminate this file')" @click="emit('terminate', [task.id])"><AppIcon name="stop" :size="16" /></button>
            <button v-if="task.status === 'failed' && !task.retryBlocked" class="record-icon-btn" type="button" :title="locale.text('重试该文件', 'Retry this file')" :aria-label="locale.text('重试该文件', 'Retry this file')" @click="emit('retry', task.id)"><AppIcon name="retry" :size="16" /></button>
            <button v-if="task.status === 'failed'" class="record-icon-btn danger" type="button" :title="locale.text('删除失败记录', 'Delete failed record')" :aria-label="locale.text('删除失败记录', 'Delete failed record')" @click="emit('removeFailed', task.id)"><AppIcon name="delete" :size="16" /></button>
            <button v-if="task.status === 'succeeded' || task.status === 'cancelled'" class="record-icon-btn danger" type="button" :title="locale.text('删除该任务记录', 'Delete this task record')" :aria-label="locale.text('删除该任务记录', 'Delete this task record')" @click="emit('clear', [task.id])"><AppIcon name="delete" :size="16" /></button>
          </div>
        </article>
      </div>
      <div v-else class="upload-empty-state"><AppIcon name="upload" :size="28" /><strong>{{ tasks.length ? locale.text('当前筛选下没有任务', 'No tasks match this filter') : locale.text('选择文件或拖拽到上方开始上传', 'Choose or drop files above to start uploading') }}</strong></div>
    </section>
  </div>
</template>

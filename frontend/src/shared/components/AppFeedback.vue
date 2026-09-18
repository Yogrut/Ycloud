<script setup lang="ts">
import { onBeforeUnmount, watch } from 'vue'
import { activeFeedback } from '../composables/feedback'
import AppToast from './AppToast.vue'

const props = withDefaults(defineProps<{ message?: string; kind?: 'success' | 'error'; revision?: number }>(), { message: '', kind: 'error', revision: 0 })
const id = Symbol('feedback')
function dismiss(): void {
  if (activeFeedback.value === id) activeFeedback.value = undefined
}
watch(() => [props.message, props.kind, props.revision], () => {
  if (props.message) activeFeedback.value = id
  else dismiss()
}, { immediate: true })
onBeforeUnmount(dismiss)
</script>

<template>
  <AppToast v-if="message && activeFeedback === id" :key="revision" :message="message" :kind="kind" @close="dismiss" />
</template>

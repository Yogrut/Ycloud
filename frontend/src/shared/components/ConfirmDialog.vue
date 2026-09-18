<script setup lang="ts">
import AppFeedback from './AppFeedback.vue'
import { nextTick, onBeforeUnmount, onMounted, ref, useId } from 'vue'
import { useLocale } from '../i18n'
import AppIcon from './AppIcon.vue'

const props = defineProps<{ title: string; message: string; target?: string; detail?: string; error?: string; busy?: boolean; confirmLabel?: string; danger?: boolean }>()
const emit = defineEmits<{ close: []; confirm: [] }>()
const locale = useLocale()
const id = useId()
const panel = ref<HTMLElement>()
const cancelButton = ref<HTMLButtonElement>()
let previous: HTMLElement | null = null
let overflow = ''
function close(): void { if (!props.busy) emit('close') }
function keydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close() }
  if (event.key !== 'Tab') return
  const controls = Array.from(panel.value?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])
  const first = controls[0]
  const last = controls.at(-1)
  if (!first) { event.preventDefault(); return }
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
}
onMounted(async () => {
  previous = document.activeElement as HTMLElement | null
  overflow = document.body.style.overflow
  document.body.style.overflow = 'hidden'
  await nextTick()
  cancelButton.value?.focus()
})
onBeforeUnmount(() => { document.body.style.overflow = overflow; previous?.focus() })
</script>

<template>
  <div class="overlay confirmation-overlay" @click.self="close">
    <section ref="panel" class="modal confirmation-dialog" role="dialog" aria-modal="true" :aria-labelledby="`${id}-title`" :aria-describedby="`${id}-message`" tabindex="-1" @keydown="keydown">
      <header class="confirmation-head">
        <h2 :id="`${id}-title`">{{ title }}</h2>
        <button class="confirmation-close" type="button" :disabled="busy" :aria-label="locale.t('common.close')" @click="close"><AppIcon name="close" :size="18" weight="regular" /></button>
      </header>
      <div class="confirmation-body">
        <div :id="`${id}-message`" class="confirmation-warning"><span class="confirmation-warning-icon" aria-hidden="true">!</span><span>{{ message }}</span></div>
        <p v-if="detail" class="confirmation-detail">{{ detail }}</p>
        <ul v-if="target" class="confirmation-target"><li>{{ target }}</li></ul>
        <AppFeedback :message="error" />
      </div>
      <footer class="confirmation-actions">
        <button ref="cancelButton" class="btn secondary" type="button" :disabled="busy" @click="close">{{ locale.t('common.cancel') }}</button>
        <button class="btn" :class="{ danger }" type="button" :disabled="busy" @click="!busy && emit('confirm')">{{ busy ? locale.text('处理中…', 'Working…') : (confirmLabel || locale.t('common.confirm')) }}</button>
      </footer>
    </section>
  </div>
</template>

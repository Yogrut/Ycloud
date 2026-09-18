<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, useId } from 'vue'
import { useLocale } from '../i18n'

const props = defineProps<{ title: string; busy?: boolean; wide?: boolean }>()
const emit = defineEmits<{ close: [] }>()
const locale = useLocale()
const titleId = useId()
const panel = ref<HTMLElement>()
let previous: HTMLElement | null = null
let previousOverflow = ''
function close(): void { if (!props.busy) emit('close') }
function keydown(event: KeyboardEvent): void {
  if (event.defaultPrevented) return
  if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close() }
  if (event.key !== 'Tab') return
  const controls = Array.from(panel.value?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]') ?? []).filter(el => !el.closest('[hidden]'))
  const first = controls[0]
  const last = controls.at(-1)
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus() }
}
onMounted(async () => {
  previous = document.activeElement as HTMLElement | null
  previousOverflow = document.body.style.overflow
  document.body.style.overflow = 'hidden'
  await nextTick()
  panel.value?.querySelector<HTMLElement>('.settings-drawer-content input:not(:disabled):not([readonly]), .settings-drawer-content button:not(:disabled)')?.focus()
})
onBeforeUnmount(() => {
  document.body.style.overflow = previousOverflow
  previous?.focus()
})
</script>

<template>
  <div class="settings-drawer-backdrop" @click.self="close">
    <section ref="panel" class="settings-drawer" :class="{ wide }" role="dialog" aria-modal="true" :aria-labelledby="titleId" @keydown="keydown">
      <header class="settings-drawer-head">
        <h2 :id="titleId">{{ title }}</h2>
        <button class="drawer-close" type="button" :disabled="busy" :aria-label="locale.t('common.close')" @click="close">×</button>
      </header>
      <div class="settings-drawer-content"><slot /></div>
    </section>
  </div>
</template>

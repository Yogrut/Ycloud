<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useId } from 'vue'

type SelectValue = string | number
type SelectOption = { value: SelectValue; label: string; disabled?: boolean }

const props = withDefaults(defineProps<{
  modelValue: SelectValue
  options: SelectOption[]
  label: string
  disabled?: boolean
  placement?: 'top' | 'bottom'
}>(), {
  disabled: false,
  placement: 'bottom',
})

const emit = defineEmits<{
  'update:modelValue': [value: SelectValue]
  change: [value: SelectValue]
}>()

const root = ref<HTMLElement>()
const trigger = ref<HTMLButtonElement>()
const optionButtons = ref<HTMLButtonElement[]>([])
const open = ref(false)
const activeIndex = ref(-1)
const listboxId = `app-select-${useId()}`

const selectedIndex = computed(() => props.options.findIndex(option => option.value === props.modelValue))
const selectedLabel = computed(() => props.options[selectedIndex.value]?.label ?? '')

function usableIndex(start: number, direction: 1 | -1): number {
  if (!props.options.length) return -1
  let index = start
  for (let checked = 0; checked < props.options.length; checked += 1) {
    index = (index + direction + props.options.length) % props.options.length
    if (!props.options[index]?.disabled) return index
  }
  return -1
}

async function show(preferredIndex = selectedIndex.value): Promise<void> {
  if (props.disabled || !props.options.length) return
  open.value = true
  activeIndex.value = preferredIndex >= 0 && !props.options[preferredIndex]?.disabled
    ? preferredIndex
    : usableIndex(-1, 1)
  await nextTick()
  optionButtons.value[activeIndex.value]?.focus({ preventScroll: true })
}

function hide(restoreFocus = false): void {
  open.value = false
  activeIndex.value = -1
  if (restoreFocus) nextTick(() => trigger.value?.focus({ preventScroll: true }))
}

function toggle(): void {
  if (open.value) hide()
  else void show()
}

function choose(option: SelectOption): void {
  if (option.disabled) return
  if (option.value !== props.modelValue) {
    emit('update:modelValue', option.value)
    emit('change', option.value)
  }
  hide(true)
}

function move(direction: 1 | -1): void {
  const next = usableIndex(activeIndex.value, direction)
  if (next < 0) return
  activeIndex.value = next
  nextTick(() => optionButtons.value[next]?.focus({ preventScroll: true }))
}

function handleTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    void show(event.key === 'ArrowDown' ? usableIndex(-1, 1) : usableIndex(0, -1))
  }
}

function handleOptionKeydown(event: KeyboardEvent, option: SelectOption): void {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    move(event.key === 'ArrowDown' ? 1 : -1)
  } else if (event.key === 'Home' || event.key === 'End') {
    event.preventDefault()
    const next = event.key === 'Home' ? usableIndex(-1, 1) : usableIndex(0, -1)
    if (next >= 0) {
      activeIndex.value = next
      nextTick(() => optionButtons.value[next]?.focus({ preventScroll: true }))
    }
  } else if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    choose(option)
  } else if (event.key === 'Escape' || event.key === 'Tab') {
    if (event.key === 'Escape') event.preventDefault()
    hide(event.key === 'Escape')
  }
}

function closeFromOutside(event: PointerEvent): void {
  if (open.value && !root.value?.contains(event.target as Node)) hide()
}

onMounted(() => document.addEventListener('pointerdown', closeFromOutside))
onBeforeUnmount(() => document.removeEventListener('pointerdown', closeFromOutside))
</script>

<template>
  <div ref="root" class="app-select" :class="[`placement-${placement}`, { open, disabled }]">
    <button
      ref="trigger"
      class="app-select-trigger"
      type="button"
      :aria-label="label"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-controls="listboxId"
      :disabled="disabled"
      @click="toggle"
      @keydown="handleTriggerKeydown"
    >
      <span class="app-select-value">{{ selectedLabel }}</span>
      <span class="app-select-chevron" aria-hidden="true" />
    </button>
    <div v-if="open" :id="listboxId" class="app-select-menu" role="listbox" :aria-label="label">
      <button
        v-for="(option, index) in options"
        :key="`${typeof option.value}-${option.value}`"
        :ref="element => { if (element) optionButtons[index] = element as HTMLButtonElement }"
        class="app-select-option"
        :class="{ selected: option.value === modelValue, active: index === activeIndex }"
        type="button"
        role="option"
        :aria-selected="option.value === modelValue"
        :disabled="option.disabled"
        @click="choose(option)"
        @focus="activeIndex = index"
        @keydown="handleOptionKeydown($event, option)"
      >
        {{ option.label }}
      </button>
    </div>
  </div>
</template>

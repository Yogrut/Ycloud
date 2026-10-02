<script setup lang="ts">
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { computed, reactive, ref, watch } from 'vue'
import SettingRow from '../../shared/components/SettingRow.vue'
import SettingsDrawer from '../../shared/components/SettingsDrawer.vue'
import type { AdminInfo, UpdateLoginSecuritySettingsRequest } from '../../shared/api/admin'
import { updateLoginSecuritySettings } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'
import { LOGIN_PROTECTION_LIMITS, buildLoginProtectionRequest, loginProtectionChanges, loginProtectionDraft } from './loginProtectionForm'

const props = defineProps<{ info: AdminInfo }>()
const emit = defineEmits<{ saved: [message: string] }>()
const locale = useLocale()
const feedbackRevision = ref(0)
const draft = reactive(loginProtectionDraft(props.info))
const inputFields = ['adminFailures', 'adminBlockMinutes', 'webFailures', 'webBlockMinutes'] as const
const saving = ref(false)
const errorMessage = ref('')
const editor = ref<number>()
const fields = computed(() => {
  const current = loginProtectionDraft(props.info)
  return [
    { label: locale.text('管理员错误次数', 'Administrator failure limit'), value: current.adminFailures, ...LOGIN_PROTECTION_LIMITS.adminFailures, unit: locale.text('次', 'attempts') },
    { label: locale.text('管理员封禁时间', 'Administrator block duration'), value: current.adminBlockMinutes, ...LOGIN_PROTECTION_LIMITS.blockMinutes, unit: locale.text('分钟', 'minutes') },
    { label: locale.text('首页错误次数', 'Browser failure limit'), value: current.webFailures, ...LOGIN_PROTECTION_LIMITS.webFailures, unit: locale.text('次', 'attempts') },
    { label: locale.text('首页封禁时间', 'Browser block duration'), value: current.webBlockMinutes, ...LOGIN_PROTECTION_LIMITS.blockMinutes, unit: locale.text('分钟', 'minutes') },
  ]
})
const activeValue = computed({
  get: () => draft[inputFields[editor.value ?? 0]!],
  set: value => { draft[inputFields[editor.value ?? 0]!] = value },
})
const hasChanges = computed(() => Object.keys(loginProtectionChanges(draft, props.info)).length > 0)

function openEditor(index: number): void {
  reset()
  editor.value = index
}

function reset(): void {
  Object.assign(draft, loginProtectionDraft(props.info))
  errorMessage.value = ''
}

watch(() => props.info, reset)

async function submit(): Promise<void> {
  if (saving.value || !hasChanges.value) return
  let body: UpdateLoginSecuritySettingsRequest
  try {
    body = buildLoginProtectionRequest(draft, props.info, locale.text)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('登录保护设置无效', 'Invalid sign-in protection settings')
    return
  }
  saving.value = true
  errorMessage.value = ''
  try {
    await updateLoginSecuritySettings(body)
    editor.value = undefined
    emit('saved', locale.text('登录保护设置已保存', 'Sign-in protection settings saved'))
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : locale.text('保存失败', 'Unable to save changes')
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <section class="admin-pane form-pane glass" aria-labelledby="protection-title">
    <header class="admin-pane-head">
      <div>
        <h1 id="protection-title">{{ locale.text('登录保护', 'Sign-in protection') }}</h1>
        <p>{{ locale.text('设置连续登录失败后的自动封禁阈值和封禁时间。', 'Set the automatic restriction threshold and duration after consecutive sign-in failures.') }}</p>
      </div>
    </header>
    <div class="settings-rows">
      <SettingRow v-for="(field, index) in fields" :key="index" :label="field.label" :value="`${field.value} ${field.unit}`" @edit="openEditor(index)" />
    </div>
  </section>
  <SettingsDrawer v-if="editor !== undefined" :title="fields[editor]!.label" :busy="saving" @close="editor = undefined">
    <form class="drawer-form" @submit.prevent="feedbackRevision++; submit()">
      <label>{{ fields[editor]!.label }}<input v-model="activeValue" class="input" type="number" :min="fields[editor]!.min" :max="fields[editor]!.max" step="1" required></label>
      <p class="field-hint">{{ fields[editor]!.min }}–{{ fields[editor]!.max }} {{ fields[editor]!.unit }}</p>
      <AppFeedback :revision="feedbackRevision" :message="errorMessage" />
      <div class="modal-actions"><button class="btn secondary" type="button" :disabled="saving" @click="editor = undefined">{{ locale.t('common.cancel') }}</button><button class="btn" type="submit" :disabled="saving || !hasChanges">{{ locale.text('确认', 'Confirm') }}</button></div>
    </form>
  </SettingsDrawer>
</template>

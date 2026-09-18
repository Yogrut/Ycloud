<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { getIdentity, type Identity } from '../../shared/api/auth'
import { userLogin, type BrowserCapabilities } from '../../shared/api/browser'
import AppIcon from '../../shared/components/AppIcon.vue'
import AppFeedback from '../../shared/components/AppFeedback.vue'
import { useLocale } from '../../shared/i18n'

const props = defineProps<{ storageName: string; capabilities: BrowserCapabilities }>()
const emit = defineEmits<{ signedIn: []; signOut: []; closed: [] }>()
const locale = useLocale()
const identity = ref<Identity | null>(null)
const visible = ref(false)
const username = ref('')
const password = ref('')
const error = ref('')
const feedbackRevision = ref(0)
const busy = ref(false)
const dialog = ref<HTMLElement>()
const trigger = ref<HTMLButtonElement>()
const keyboardNavigation = ref(false)
const profilePosition = ref({ left: '0px', top: '0px' })
const signedIn = computed(() => identity.value?.logged_in && !identity.value.is_admin && !!identity.value.username)
const permissions = computed(() => {
  const labels: Array<[keyof BrowserCapabilities, string]> = [
    ['download', locale.text('下载', 'Download')], ['upload', locale.text('上传', 'Upload')],
    ['create_directory', locale.text('新建文件夹', 'Create folders')], ['rename', locale.text('重命名', 'Rename')],
    ['move_items', locale.text('移动', 'Move')], ['copy', locale.text('复制', 'Copy')], ['delete', locale.text('删除', 'Delete')],
  ]
  return labels.filter(([key]) => props.capabilities[key]).map(([, label]) => label)
})

async function refreshIdentity(): Promise<void> {
  try { identity.value = await getIdentity() } catch { identity.value = null }
}
function open(): void {
  if (visible.value) { close(); return }
  username.value = ''
  password.value = ''
  error.value = ''
  visible.value = true
  void refreshIdentity()
  void nextTick(() => {
    placeProfile()
    const target = dialog.value?.querySelector<HTMLElement>('input') ?? dialog.value?.querySelector<HTMLElement>('button')
    target?.focus()
  })
}
function placeProfile(): void {
  if (!visible.value || !signedIn.value) return
  const bounds = trigger.value?.getBoundingClientRect()
  if (!bounds) return
  const width = dialog.value?.getBoundingClientRect().width ?? 200
  profilePosition.value = {
    left: `${Math.max(12, Math.min(bounds.left + (bounds.width - width) / 2, window.innerWidth - width - 12))}px`,
    top: `${bounds.bottom + 8}px`,
  }
}
watch([visible, signedIn, permissions, () => props.storageName, () => identity.value?.username], placeProfile, { flush: 'post' })
function dismissOutside(event: Event): void {
  if (!visible.value || !signedIn.value || busy.value) return
  const target = event.target as Node | null
  if (target && (trigger.value?.contains(target) || dialog.value?.contains(target))) return
  visible.value = false
  emit('closed')
}
function handleEscape(event: KeyboardEvent): void {
  // Typing or submitting a form must not opt into navigation focus rings.
  // Keep Tab mode until the next pointer interaction, including focus return.
  if (event.key === 'Tab') keyboardNavigation.value = true
  if (event.key === 'Escape' && visible.value) close()
}
function handlePointer(event: Event): void {
  keyboardNavigation.value = false
  dismissOutside(event)
}
function close(): void {
  if (busy.value) return
  visible.value = false
  password.value = ''
  emit('closed')
  trigger.value?.focus()
}
function trapFocus(event: KeyboardEvent): void {
  if (event.key !== 'Tab') return
  const elements = [...(dialog.value?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), [tabindex="0"]') ?? [])]
  const first = elements[0]
  const last = elements.at(-1)
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus() }
}
async function submit(): Promise<void> {
  if (busy.value) return
  feedbackRevision.value++
  if (!username.value.trim() || !password.value) {
    error.value = locale.text('请输入用户名和密码', 'Enter your username and password')
    return
  }
  busy.value = true
  error.value = ''
  try {
    const result = await userLogin(username.value.trim(), password.value)
    if (!result.success || result.is_admin) throw new Error(result.message ?? locale.text('登录失败', 'Sign-in failed'))
    await refreshIdentity()
    password.value = ''
    visible.value = false
    trigger.value?.focus()
    emit('signedIn')
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : locale.text('登录失败', 'Sign-in failed')
  } finally { busy.value = false }
}
onMounted(() => {
  void refreshIdentity()
  document.addEventListener('pointerdown', handlePointer)
  document.addEventListener('focusin', dismissOutside)
  document.addEventListener('keydown', handleEscape)
  window.addEventListener('resize', placeProfile)
  window.addEventListener('scroll', placeProfile, true)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', handlePointer)
  document.removeEventListener('focusin', dismissOutside)
  document.removeEventListener('keydown', handleEscape)
  window.removeEventListener('resize', placeProfile)
  window.removeEventListener('scroll', placeProfile, true)
})
defineExpose({ open, refreshIdentity })
</script>

<template>
  <button ref="trigger" class="icon-btn flat user-account-trigger" :class="{ 'keyboard-focus': keyboardNavigation }" type="button" :title="signedIn ? locale.text('用户信息', 'User information') : locale.text('用户登录', 'User sign-in')" :aria-label="signedIn ? locale.text('用户信息', 'User information') : locale.text('用户登录', 'User sign-in')" :aria-expanded="visible" aria-haspopup="dialog" @click="open">
    <AppIcon name="account" :size="24" />
  </button>
  <Teleport to="body">
    <section v-if="visible && signedIn" ref="dialog" class="user-account-popover" :style="profilePosition" role="dialog" :aria-label="locale.text('用户信息', 'User information')">
      <div class="user-account-summary"><AppIcon name="account" :size="30" /><div><strong>{{ identity?.username }}</strong><p>{{ locale.text('普通用户', 'User') }}</p></div></div>
      <dl class="user-account-details">
        <dt>{{ locale.text('当前存储', 'Current storage') }}</dt><dd>{{ storageName || '—' }}</dd>
        <dt>{{ locale.text('操作权限', 'Permissions') }}</dt><dd>{{ permissions.join('、') || locale.text('仅浏览', 'Browse only') }}</dd>
      </dl>
      <button class="user-account-signout" :class="{ 'keyboard-focus': keyboardNavigation }" type="button" @click="emit('signOut')"><AppIcon name="sign-out" :size="17" />{{ locale.text('退出登录', 'Sign out') }}</button>
    </section>
    <div v-else-if="visible" class="overlay active user-account-overlay" @click.self="close" @keydown.esc="close">
      <section ref="dialog" class="modal user-account-modal" role="dialog" aria-modal="true" aria-labelledby="user-account-title" @keydown="trapFocus">
        <div class="user-account-heading">
          <div class="user-login-brand"><AppIcon name="cloud" :size="28" /><span>Ycloud</span></div>
          <button class="user-login-close" type="button" :disabled="busy" :aria-label="locale.text('关闭', 'Close')" @click="close">×</button>
        </div>
        <div class="user-login-intro">
          <h2 id="user-account-title">{{ locale.text('用户登录', 'User sign-in') }}</h2>
          <p>{{ locale.text('登录以访问你的文件空间', 'Sign in to your file space') }}</p>
        </div>
        <form class="user-login-form" @submit.prevent="submit">
          <label>{{ locale.text('用户名', 'Username') }}<input v-model="username" class="input" autocomplete="username" autofocus :disabled="busy" :placeholder="locale.text('输入用户名', 'Username')"></label>
          <label>{{ locale.text('密码', 'Password') }}<input v-model="password" class="input" type="password" autocomplete="current-password" :disabled="busy" :placeholder="locale.text('输入密码', 'Password')"></label>
          <AppFeedback :message="error" :revision="feedbackRevision" />
          <button class="btn user-login-submit" type="submit" :disabled="busy">{{ busy ? locale.text('登录中…', 'Signing in…') : locale.text('登录', 'Sign in') }}</button>
        </form>
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.user-account-trigger { display: flex; align-items: center; justify-content: center; }
.user-account-trigger:focus, .user-account-signout:focus { outline: none; }
.user-account-trigger.keyboard-focus:focus, .user-account-signout.keyboard-focus:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
.user-account-trigger svg { flex-shrink: 0; }
.user-account-heading { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding-bottom: 18px; border-bottom: 1px solid var(--line); }
.modal.user-account-modal { position: relative; width: min(420px, 100%); box-sizing: border-box; padding: 28px 30px 30px; max-height: calc(100dvh - 36px); overflow-y: auto; border-radius: 14px; }
.user-login-brand { display: flex; align-items: center; gap: 9px; font-size: 17px; font-weight: 700; }
.user-login-brand svg { color: var(--accent); }
.user-login-intro { margin: 22px 0 24px; }
.user-login-intro h2 { margin: 0; font-size: 24px; line-height: 1.3; letter-spacing: -.025em; }
.user-login-intro p { margin: 7px 0 0; font-size: 13px; line-height: 1.6; }
.user-login-form { display: grid; gap: 18px; }
.user-login-form label { margin: 0; gap: 8px; color: var(--text); font-size: 13px; font-weight: 650; }
.user-login-form .input { min-height: 42px; height: 42px; padding: 0 12px; background: var(--panel-soft); border-color: var(--line); border-radius: 8px; }
.user-login-form .input:not([type="password"]) { font-size: 14px; letter-spacing: normal; }
.user-login-form .input::placeholder { color: var(--muted); font-size: 13px; font-weight: 400; letter-spacing: normal; }
.user-login-form .user-login-submit { width: 100%; min-height: 42px; margin-top: 2px; border-radius: 8px; }
.user-account-popover { position: fixed; z-index: 90; box-sizing: border-box; width: max-content; min-width: min(200px, calc(100vw - 24px)); max-width: min(360px, calc(100vw - 24px)); padding: 14px 12px; color: var(--text); background: var(--panel); border: 1px solid var(--line); border-radius: 12px; box-shadow: var(--shadow); max-height: calc(100dvh - 96px); overflow-y: auto; }
.user-account-summary { display: flex; align-items: center; gap: 10px; margin-bottom: 16px; font-size: 14px; }
.user-account-summary svg { color: var(--accent); }
.user-account-summary strong { overflow-wrap: anywhere; }
.user-account-summary p { margin: 5px 0 0; font-size: 13px; }
.user-account-details { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 10px 12px; margin: 0; font-size: 12px; line-height: 1.6; }
.user-account-details dt { color: var(--muted); }
.user-account-details dd { margin: 0; overflow-wrap: anywhere; }
.user-account-signout { display: flex; align-items: center; gap: 8px; width: 100%; margin-top: 14px; padding: 12px 0 0; border: 0; border-top: 1px solid var(--line); background: transparent; color: var(--accent); cursor: pointer; font: inherit; font-size: 13px; }
</style>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import LoginView from './LoginView.vue'
import LocaleToggle from '../../shared/components/LocaleToggle.vue'
import ThemeToggle from '../../shared/components/ThemeToggle.vue'
import AppIcon from '../../shared/components/AppIcon.vue'
import { useTheme } from '../../shared/composables/useTheme'
import { currentAppPath } from '../../shared/routes'
import { lazyPage } from '../../shared/lazyPage'
import AdminView from '../admin/AdminView.vue'
import BrowserView from '../browser/BrowserView.vue'

// Only the preview page still waits for its code; the file browser and the
// console shell ship with the main program so a route switch never paints a
// page-load card.
const PreviewView = lazyPage(() => import('../preview/PreviewView.vue').then(module => module.default))

const theme = useTheme()
const activePath = ref(currentAppPath())
const route = computed(() => activePath.value)
const isBrowser = computed(() => route.value === '/browse')
const isAdmin = computed(() => route.value === '/admin' || route.value.startsWith('/admin/'))
const isPreview = computed(() => route.value === '/preview')

// boot.js paints the first frame from this attribute; keep it current so a
// client-side jump from the login page to the file browser keeps its colour.
function applyShell(path: string): void {
  document.documentElement.dataset.shell =
    path === '/browse' || path.startsWith('/browse/') ? 'browser' : 'app'
}
applyShell(activePath.value)

function syncRoute(): void {
  activePath.value = currentAppPath()
  applyShell(activePath.value)
}

function suppressNativeContextMenu(event: MouseEvent): void {
  event.preventDefault()
}

// Warm the chunks that a page switch would otherwise have to download. Code
// only: no request touches the API here. Console sections are warmed separately
// once an administrator is known, so visitors never pay for them.
function prefetchPages(): void {
  const loads = [
    () => import('../preview/PreviewView.vue'),
  ]
  void (async () => {
    for (const load of loads) await load().catch(() => undefined)
  })()
}

onMounted(() => {
  window.addEventListener('popstate', syncRoute)
  document.addEventListener('contextmenu', suppressNativeContextMenu)
  const idle = window.requestIdleCallback
  if (typeof idle === 'function') idle.call(window, () => prefetchPages(), { timeout: 2_000 })
  else window.setTimeout(prefetchPages, 1_200)
})
onBeforeUnmount(() => {
  window.removeEventListener('popstate', syncRoute)
  document.removeEventListener('contextmenu', suppressNativeContextMenu)
})
</script>

<template>
  <AdminView v-if="isAdmin" :theme="theme" />
  <PreviewView v-else-if="isPreview" :theme="theme" />
  <BrowserView v-else-if="isBrowser" :theme="theme" />
  <template v-else>
    <div class="login-page">
      <header class="login-header">
        <div class="login-header-brand"><AppIcon name="cloud" :size="28" /><span>Ycloud</span></div>
        <div class="login-header-actions">
          <LocaleToggle />
          <ThemeToggle :theme="theme.current.value" @toggle="theme.toggle" />
        </div>
      </header>
      <LoginView />
    </div>
  </template>
</template>

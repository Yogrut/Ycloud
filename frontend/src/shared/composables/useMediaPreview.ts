import { readonly, ref, watch, type Ref } from 'vue'
import { isPreviewTrafficExhausted } from '../api/browser'

interface MediaTarget {
  url: string
  controller: AbortController
  checking: boolean
}

export function useMediaPreview(url: Readonly<Ref<string>>, checkBeforeLoad: Readonly<Ref<boolean>> = ref(false)) {
  const failed = ref(false)
  const trafficExhausted = ref(false)
  const ready = ref(false)
  let current: MediaTarget | undefined

  async function checkTraffic(target: MediaTarget, beforeLoad = false): Promise<void> {
    if (target.checking) return
    target.checking = true
    try {
      // This diagnoses quota denial, not whether a browser can render the file.
      const exhausted = await isPreviewTrafficExhausted(target.url, target.controller.signal)
      if (target.controller.signal.aborted) return
      trafficExhausted.value = exhausted
      if (exhausted) failed.value = true
      else if (beforeLoad && !failed.value) ready.value = true
    } finally {
      target.checking = false
    }
  }

  watch([url, checkBeforeLoad], ([source, beforeLoad], _previous, onCleanup) => {
    failed.value = false
    trafficExhausted.value = false
    ready.value = false
    current = undefined
    if (!source) return
    const target: MediaTarget = { url: source, controller: new AbortController(), checking: false }
    current = target
    onCleanup(() => {
      target.controller.abort()
      if (current === target) current = undefined
    })
    if (beforeLoad) void checkTraffic(target, true)
    else ready.value = true
  }, { immediate: true, flush: 'sync' })

  async function onError(): Promise<void> {
    const target = current
    if (!target || failed.value || target.controller.signal.aborted) return
    failed.value = true
    ready.value = false
    await checkTraffic(target)
  }

  return { failed: readonly(failed), trafficExhausted: readonly(trafficExhausted), ready: readonly(ready), onError }
}

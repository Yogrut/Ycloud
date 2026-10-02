import { readonly, ref, watch, type Ref } from 'vue'
import { ApiError } from '../api/client'
import { readTextPreview } from '../api/textPreview'
import { useLocale } from '../i18n'

// An empty URL disables reading; each target owns one cancellable request.
export function useTextPreview(url: Readonly<Ref<string>>) {
  const text = ref('')
  const error = ref('')
  const truncated = ref(false)
  const ready = ref(false)
  const trafficExhausted = ref(false)

  watch(url, (source, _previous, onCleanup) => {
    text.value = ''
    error.value = ''
    truncated.value = false
    ready.value = false
    trafficExhausted.value = false
    if (!source) return

    const controller = new AbortController()
    onCleanup(() => controller.abort())
    void readTextPreview(source, controller.signal).then(result => {
      if (controller.signal.aborted) return
      text.value = result.text
      truncated.value = result.truncated
      ready.value = true
    }).catch(reason => {
      if (controller.signal.aborted) return
      error.value = reason instanceof Error ? reason.message : useLocale().t('preview.failed')
      trafficExhausted.value = reason instanceof ApiError && reason.status === 429
    })
  }, { immediate: true, flush: 'sync' })

  return {
    text: readonly(text),
    error: readonly(error),
    truncated: readonly(truncated),
    ready: readonly(ready),
    trafficExhausted: readonly(trafficExhausted),
  }
}

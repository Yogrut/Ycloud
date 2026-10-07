import { onScopeDispose, ref, watch } from 'vue'
import { getDomainBinding, removeDomainBinding, saveDomainBinding, type DomainBindingView } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

function bindingSnapshot(view?: DomainBindingView): DomainBindingView {
  const binding = view?.binding ? { ...view.binding } : null
  if (binding?.trusted_proxy_ips) binding.trusted_proxy_ips = [...binding.trusted_proxy_ips]
  return { binding, source: view?.source ?? 'none' }
}

export function useDomainBinding(getInitial: () => DomainBindingView | undefined) {
  const locale = useLocale()
  const status = ref(bindingSnapshot(getInitial()))
  const open = ref(false)
  const busy = ref(false)
  const url = ref('')
  const proxyIps = ref('')
  const error = ref('')
  const message = ref('')
  let disposed = false
  let readRequest: AbortController | undefined

  watch(getInitial, value => { if (value && !busy.value) status.value = bindingSnapshot(value) })

  function syncDraft(): void {
    url.value = status.value.binding?.public_url ?? ''
    proxyIps.value = (status.value.binding?.trusted_proxy_ips ?? []).join(', ')
  }

  async function show(): Promise<void> {
    if (disposed || busy.value) return
    open.value = true
    error.value = ''
    message.value = ''
    busy.value = true
    const controller = new AbortController()
    readRequest = controller
    try {
      const result = await getDomainBinding(controller.signal)
      if (!disposed) status.value = bindingSnapshot(result)
    } catch (reason) {
      if (!disposed) error.value = reason instanceof Error ? reason.message : locale.text('无法读取域名设置', 'Unable to load domain settings')
    } finally {
      readRequest = undefined
      if (!disposed) syncDraft()
      busy.value = false
    }
  }

  async function submit(): Promise<void> {
    if (disposed || busy.value || !open.value) return
    busy.value = true
    error.value = ''
    message.value = ''
    const publicUrl = url.value.trim()
    const isRemoval = !publicUrl
    try {
      const result = isRemoval ? await removeDomainBinding() : await saveDomainBinding({
        public_url: publicUrl,
        trusted_proxy_ips: proxyIps.value.split(/[,，\s]+/).filter(Boolean),
      })
      if (disposed) return
      status.value = bindingSnapshot(result)
      syncDraft()
      message.value = isRemoval
        ? locale.text('已解除域名绑定，Ycloud 已恢复 HTTP 访问模式。', 'Domain binding removed. Ycloud is using HTTP access again.')
        : locale.text('域名绑定已立即生效，请使用绑定域名访问。', 'Domain binding is active. Use the bound domain to access Ycloud.')
    } catch (reason) {
      if (!disposed) error.value = reason instanceof Error ? reason.message : locale.text('保存失败', 'Unable to save changes')
    } finally {
      busy.value = false
    }
  }

  function cancel(): void {
    if (disposed || busy.value) return
    open.value = false
    syncDraft()
    error.value = ''
    message.value = ''
  }

  onScopeDispose(() => {
    disposed = true
    readRequest?.abort()
    open.value = false
    url.value = ''
    proxyIps.value = ''
    error.value = ''
    message.value = ''
    // Submitted writes keep running; their late results must not revive this editor.
  })

  return { status, open, busy, url, proxyIps, error, message, show, submit, cancel }
}

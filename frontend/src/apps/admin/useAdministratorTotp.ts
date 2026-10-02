import { onScopeDispose, ref, watch } from 'vue'
import { disableAdministratorTotp, enableAdministratorTotp, setupAdministratorTotp } from '../../shared/api/admin'
import { useLocale } from '../../shared/i18n'

type TotpSetup = Awaited<ReturnType<typeof setupAdministratorTotp>>

export function useAdministratorTotp(getEnabled: () => boolean | undefined, onDisabled: () => void) {
  const locale = useLocale()
  const enabled = ref(Boolean(getEnabled()))
  const password = ref('')
  const code = ref('')
  const setup = ref<TotpSetup>()
  const recoveryCodes = ref<string[]>([])
  const busy = ref(false)
  const error = ref('')
  const copyNotice = ref<{ message: string; kind: 'success' | 'error' }>()
  let disposed = false

  function clearSecrets(): void {
    password.value = ''
    code.value = ''
    setup.value = undefined
    recoveryCodes.value = []
    error.value = ''
    copyNotice.value = undefined
  }

  function reset(): void {
    if (busy.value) return
    clearSecrets()
  }

  async function copySecret(): Promise<void> {
    if (disposed || !setup.value) return
    const secret = setup.value.secret
    copyNotice.value = undefined
    try {
      await navigator.clipboard.writeText(secret)
      if (disposed || setup.value?.secret !== secret) return
      copyNotice.value = { message: locale.text('密钥已复制', 'Secret copied'), kind: 'success' }
    } catch {
      if (disposed || setup.value?.secret !== secret) return
      copyNotice.value = { message: locale.text('无法复制，请使用二维码或手动记录密钥', 'Unable to copy. Scan the QR code or transcribe the secret.'), kind: 'error' }
    }
  }

  async function beginSetup(): Promise<void> {
    if (disposed || !password.value || busy.value) return
    busy.value = true
    error.value = ''
    try {
      const result = await setupAdministratorTotp(password.value)
      if (disposed) return
      setup.value = { ...result }
    } catch (reason) {
      if (!disposed) error.value = reason instanceof Error ? reason.message : locale.text('无法创建两步验证配置', 'Unable to prepare two-step verification')
    } finally {
      busy.value = false
    }
  }

  async function enable(): Promise<void> {
    if (disposed || !setup.value || !password.value || !code.value || busy.value) return
    busy.value = true
    error.value = ''
    try {
      const result = await enableAdministratorTotp(password.value, setup.value.secret, code.value.trim())
      if (disposed) return
      // Keep recovery codes visible until the user explicitly closes the editor.
      // The old admin session is revoked by the server, but closing it now would
      // prevent the user from saving these one-time codes.
      recoveryCodes.value = [...result.recovery_codes]
      enabled.value = true
      setup.value = undefined
      code.value = ''
      password.value = ''
      copyNotice.value = undefined
    } catch (reason) {
      if (!disposed) error.value = reason instanceof Error ? reason.message : locale.text('启用两步验证失败', 'Unable to enable two-step verification')
    } finally {
      busy.value = false
    }
  }

  async function disable(): Promise<void> {
    if (disposed || !password.value || !code.value || busy.value) return
    busy.value = true
    error.value = ''
    try {
      await disableAdministratorTotp(password.value, code.value.trim())
      if (disposed) return
      enabled.value = false
      clearSecrets()
      onDisabled()
    } catch (reason) {
      if (!disposed) error.value = reason instanceof Error ? reason.message : locale.text('停用两步验证失败', 'Unable to disable two-step verification')
    } finally {
      busy.value = false
    }
  }

  watch(getEnabled, value => {
    if (recoveryCodes.value.length === 0) enabled.value = Boolean(value)
  })
  onScopeDispose(() => {
    disposed = true
    // Do not cancel submitted management writes. Only discard local secrets and
    // prevent late responses from repopulating a page that no longer exists.
    clearSecrets()
  })

  return { enabled, password, code, setup, recoveryCodes, busy, error, copyNotice, reset, copySecret, beginSetup, enable, disable }
}

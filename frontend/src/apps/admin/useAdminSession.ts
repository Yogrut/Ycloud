import { onScopeDispose, ref } from 'vue'
import { AdminApiError, getAdminInfo, loginAdministrator, logoutSession, type AdminInfo } from '../../shared/api/admin'
import { appPath } from '../../shared/routes'
import { useLocale } from '../../shared/i18n'

const STORAGE_REFRESH_MS = 300_000
const SESSION_REDIRECT_MS = 700

export function useAdminSession(refreshStorage: boolean) {
  const locale = useLocale()
  const info = ref<AdminInfo>()
  const loading = ref(true)
  const requiresLogin = ref(false)
  const loadError = ref('')
  const username = ref('')
  const password = ref('')
  const totpCode = ref('')
  const totpRequired = ref(false)
  const loginError = ref('')
  const loggingIn = ref(false)
  const notice = ref('')
  const noticeRevision = ref(0)
  let loadRevision = 0
  let readRequest: AbortController | undefined
  let healthRefresh: ReturnType<typeof setInterval> | undefined
  let redirect: ReturnType<typeof setTimeout> | undefined
  let started = false
  let disposed = false

  async function load(background = false): Promise<void> {
    if (disposed) return
    const revision = ++loadRevision
    readRequest?.abort()
    const controller = new AbortController()
    readRequest = controller
    if (!background) loading.value = true
    loadError.value = ''
    try {
      const result = await getAdminInfo(controller.signal)
      if (revision !== loadRevision) return
      info.value = result
      requiresLogin.value = false
    } catch (reason) {
      if (revision !== loadRevision) return
      if (reason instanceof AdminApiError && (reason.status === 401 || reason.status === 403)) {
        requiresLogin.value = true
        info.value = undefined
      } else {
        loadError.value = reason instanceof Error ? reason.message : locale.text('管理后台加载失败', 'Unable to load the admin console')
      }
    } finally {
      if (revision === loadRevision) {
        readRequest = undefined
        loading.value = false
      }
    }
  }

  async function submitLogin(): Promise<void> {
    if (disposed || loggingIn.value) return
    if (!username.value.trim() || !password.value) {
      loginError.value = locale.text('请输入用户名和密码', 'Enter your username and password')
      return
    }
    loggingIn.value = true
    loginError.value = ''
    try {
      const result = await loginAdministrator(username.value.trim(), password.value, totpCode.value.trim())
      if (disposed) return
      if (result.totp_required) {
        totpRequired.value = true
        return
      }
      if (!result.success) throw new Error(result.message ?? locale.text('登录失败', 'Sign-in failed'))
      if (!result.is_admin) {
        await logoutSession()
        throw new Error(locale.text('用户账号不能进入管理后台', 'This user cannot open the admin console.'))
      }
      password.value = ''
      totpCode.value = ''
      totpRequired.value = false
      await load()
    } catch (reason) {
      if (!disposed) loginError.value = reason instanceof Error ? reason.message : locale.text('登录失败', 'Sign-in failed')
    } finally {
      loggingIn.value = false
    }
  }

  function resetTotpChallenge(): void {
    if (!totpRequired.value) return
    totpRequired.value = false
    totpCode.value = ''
    loginError.value = ''
  }

  function showNotice(message: string): void {
    if (disposed) return
    notice.value = message
    noticeRevision.value += 1
    // A confirmed write stays successful even if the subsequent read fails.
    // Keep the current page mounted while updating its configuration.
    void load(true)
  }

  function sessionExpired(): void {
    if (disposed || redirect !== undefined) return
    redirect = setTimeout(() => window.location.replace(appPath('/browse')), SESSION_REDIRECT_MS)
  }

  function start(): void {
    if (disposed || started) return
    started = true
    void load()
    if (refreshStorage) healthRefresh = setInterval(() => {
      if (!document.hidden && !requiresLogin.value && !loggingIn.value && !readRequest) void load(true)
    }, STORAGE_REFRESH_MS)
  }

  onScopeDispose(() => {
    disposed = true
    ++loadRevision
    readRequest?.abort()
    clearInterval(healthRefresh)
    clearTimeout(redirect)
    password.value = ''
    totpCode.value = ''
    // Login and logout writes are not aborted, but cannot start follow-up reads after disposal.
  })

  return {
    info, loading, requiresLogin, loadError, username, password, totpCode, totpRequired,
    loginError, loggingIn, notice, noticeRevision, load, submitLogin, resetTotpChallenge,
    showNotice, sessionExpired, start,
  }
}

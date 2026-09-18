import { ref } from 'vue'
import type { Ref } from 'vue'
import type { BrowserCapabilities, FileEntry } from '../../shared/api/browser'
import { adminLogin, logout, unlockFolder } from '../../shared/api/browser'
import { useLocale } from '../../shared/i18n'
import { appPath } from '../../shared/routes'

interface BrowserAccessContext {
  storageId: Ref<string>
  capabilities: Ref<BrowserCapabilities>
  isAdministrator: Ref<boolean>
  navigate: (destination: string) => Promise<void>
  resetAfterSignIn: (requestedStorageId: string) => Promise<void>
  openAccountMenu: () => void
  disposeListing: () => void
}

export function useBrowserAccess(context: BrowserAccessContext) {
  const locale = useLocale()
  const feedbackRevision = ref(0)
  const showUnlock = ref(false)
  const unlockPath = ref('')
  const unlockPassword = ref('')
  const unlockError = ref('')
  const showAdmin = ref(false)
  const adminUser = ref('')
  const adminPassword = ref('')
  const adminTotpCode = ref('')
  const adminTotpRequired = ref(false)
  const adminError = ref('')
  const adminLoggingIn = ref(false)
  const pendingStorageId = ref('')

  function requestStorageLogin(storageId: string): void {
    pendingStorageId.value = storageId
    context.openAccountMenu()
  }

  function openEntry(entry: FileEntry): void {
    if (entry.is_dir) {
      if (entry.locked) {
        unlockPath.value = entry.path
        unlockPassword.value = ''
        unlockError.value = ''
        showUnlock.value = true
      } else void context.navigate(entry.path)
      return
    }
    if (!context.capabilities.value.download) return
    const params = new URLSearchParams({ path: `/${entry.path}`, storage_id: context.storageId.value })
    window.open(`${appPath('/preview')}?${params.toString()}`, '_blank', 'noopener')
  }

  async function submitUnlock(): Promise<void> {
    feedbackRevision.value++
    if (!unlockPassword.value) return
    try {
      const result = await unlockFolder(unlockPath.value, unlockPassword.value, context.storageId.value)
      if (!result.success) throw new Error(result.message ?? locale.text('密码错误', 'Incorrect password'))
      showUnlock.value = false
      await context.navigate(unlockPath.value)
    } catch (error) {
      unlockError.value = error instanceof Error ? error.message : locale.text('解锁失败', 'Unable to unlock this folder')
    }
  }

  async function openAdmin(): Promise<void> {
    pendingStorageId.value = ''
    if (context.isAdministrator.value) {
      window.location.href = appPath('/admin/dashboard')
      return
    }
    adminUser.value = ''
    adminPassword.value = ''
    adminTotpCode.value = ''
    adminTotpRequired.value = false
    adminError.value = ''
    showAdmin.value = true
  }

  async function submitAdmin(): Promise<void> {
    if (adminLoggingIn.value) return
    adminError.value = ''
    if (!adminUser.value.trim() || !adminPassword.value) {
      adminError.value = locale.text('请输入用户名和密码', 'Enter your username and password')
      return
    }
    adminLoggingIn.value = true
    try {
      const result = await adminLogin(adminUser.value.trim(), adminPassword.value, adminTotpCode.value.trim())
      if (result.totp_required) {
        adminTotpRequired.value = true
        adminError.value = ''
        return
      }
      if (!result.success) throw new Error(result.message ?? locale.text('登录失败', 'Sign-in failed'))
      showAdmin.value = false
      adminPassword.value = ''
      adminTotpCode.value = ''
      adminTotpRequired.value = false
      context.isAdministrator.value = result.is_admin
      if (result.is_admin) window.location.href = appPath('/admin/dashboard')
    } catch (error) {
      adminError.value = error instanceof Error ? error.message : locale.text('登录失败', 'Sign-in failed')
    } finally {
      adminLoggingIn.value = false
    }
  }

  function resetAdminTotpChallenge(): void {
    if (!adminTotpRequired.value) return
    adminTotpRequired.value = false
    adminTotpCode.value = ''
    adminError.value = ''
  }

  async function onUserSignedIn(): Promise<void> {
    await context.resetAfterSignIn(pendingStorageId.value)
    pendingStorageId.value = ''
  }

  async function signOut(): Promise<void> {
    context.disposeListing()
    try { await logout() } finally {
      sessionStorage.setItem('ycloud-stay-signed-out', '1')
      window.location.replace(appPath('/'))
    }
  }

  return {
    adminError,
    adminLoggingIn,
    adminPassword,
    adminTotpCode,
    adminTotpRequired,
    adminUser,
    feedbackRevision,
    onUserSignedIn,
    openAdmin,
    openEntry,
    pendingStorageId,
    requestStorageLogin,
    resetAdminTotpChallenge,
    showAdmin,
    showUnlock,
    signOut,
    submitAdmin,
    submitUnlock,
    unlockError,
    unlockPassword,
  }
}

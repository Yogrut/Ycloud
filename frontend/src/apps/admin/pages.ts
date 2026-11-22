// One list of admin page chunks: AdminView builds its lazy pages from it and
// the prefetcher warms the same entries, so a new section cannot be forgotten.
export const adminPageLoaders = {
  account: () => import('./AccountView.vue').then(module => module.default),
  dashboard: () => import('./DashboardView.vue').then(module => module.default),
  limits: () => import('./LimitsView.vue').then(module => module.default),
  locks: () => import('./LocksView.vue').then(module => module.default),
  protection: () => import('./ProtectionView.vue').then(module => module.default),
  security: () => import('./SecurityView.vue').then(module => module.default),
  storage: () => import('./StorageView.vue').then(module => module.default),
  webdav: () => import('./WebDavView.vue').then(module => module.default),
  users: () => import('./UsersView.vue').then(module => module.default),
} as const

let adminPrefetchStarted = false

/// Warm the console shell and its sections. Called once an administrator is
/// known, so visitors never download console code.
export function prefetchAdminPages(): void {
  if (adminPrefetchStarted) return
  adminPrefetchStarted = true
  const loads: Array<() => Promise<unknown>> = [
    () => import('./AdminView.vue'),
    ...Object.values(adminPageLoaders),
  ]
  void (async () => {
    for (const load of loads) await load().catch(() => undefined)
  })()
}

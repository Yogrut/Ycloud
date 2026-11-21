export function appPath(path: string): string {
  return path === '/' ? '/' : `/${path.replace(/^\/+|\/+$/g, '')}`
}

export function currentAppPath(pathname = window.location.pathname): string {
  const path = pathname.replace(/\/+$/, '') || '/'
  if (path === '/index.html') return '/'
  return path
}

/**
 * Switches the in-app route without reloading the document. The shell listens
 * for `popstate`, so back/forward keep working and no blank page is painted
 * between views.
 */
export function navigateTo(path: string): void {
  const target = appPath(path)
  if (currentAppPath() === target) return
  window.history.pushState(null, '', target)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

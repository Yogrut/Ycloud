export function appPath(path: string): string {
  return path === '/' ? '/' : `/${path.replace(/^\/+|\/+$/g, '')}`
}

export function currentAppPath(pathname = window.location.pathname): string {
  const path = pathname.replace(/\/+$/, '') || '/'
  if (path === '/index.html') return '/'
  return path
}

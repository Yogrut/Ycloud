const developmentPrefix = import.meta.env.DEV ? '/v2' : ''

export function appPath(path: string): string {
  const normalized = path === '/' ? '/' : `/${path.replace(/^\/+|\/+$/g, '')}`
  return `${developmentPrefix}${normalized}`
}

export function currentAppPath(pathname = window.location.pathname): string {
  let path = pathname.replace(/\/+$/, '') || '/'
  if (path === '/index.html') return '/'
  if (path === '/browser.html') return '/browse'
  if (path === '/admin.html') return '/admin/account'
  if (path === '/preview.html') return '/preview'
  if (path === '/v2') return '/'
  if (path.startsWith('/v2/')) path = path.slice(3) || '/'
  return path
}

// Apply the saved theme and the current shell before the bundle runs, so the
// first frame already matches the page about to render. External file because
// the Content-Security-Policy (script-src 'self') blocks inline scripts.
try {
  var stored = localStorage.getItem('ycloud-theme')
  document.documentElement.dataset.theme = stored === 'dark' ? 'dark' : 'light'
} catch (error) {
  // A blocked storage API keeps the default light theme.
  document.documentElement.dataset.theme = 'light'
}
var path = location.pathname
document.documentElement.dataset.shell =
  path === '/browse' || path.indexOf('/browse/') === 0 ? 'browser' : 'app'

// Apply the saved theme before the bundle runs. External file because the
// Content-Security-Policy (script-src 'self') blocks inline scripts.
try {
  var theme = localStorage.getItem('ycloud-theme')
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme
} catch (error) { /* A blocked storage API keeps the OS preference. */ }

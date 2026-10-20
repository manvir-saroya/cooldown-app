// Apply the saved theme before first paint so there is no flash. Same logic as packages/ui/index.html.
try {
  var t = localStorage.getItem('cooldown:theme')
  var dark = t === 'dark' || ((t === null || t === 'system') && matchMedia('(prefers-color-scheme: dark)').matches)
  document.documentElement.classList.toggle('dark', dark)
} catch (e) {}

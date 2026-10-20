// Apply the saved theme before React renders, so the first paint is already the right one.
// (The desktop app does this in a script file; the phone's content security policy has no inline scripts,
// and this module runs before anything is drawn.) Same logic as packages/ui/index.html.
try {
  const stored = localStorage.getItem('cooldown:theme')
  const dark = stored === 'dark' || ((stored === null || stored === 'system') && matchMedia('(prefers-color-scheme: dark)').matches)
  document.documentElement.classList.toggle('dark', dark)
} catch {
  /* Storage can be blocked. The theme provider falls back to the system setting. */
}

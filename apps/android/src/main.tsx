import './theme-init'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { invoke, isTauri } from '@tauri-apps/api/core'
import { App, SampleSnapshotSource, type SnapshotSource } from '@cooldown/ui'
import './index.css'
import { PhoneSnapshotSource } from './PhoneSnapshotSource'

// Inside the app the Rust side serves what the paired PC synced. Opened in a plain browser
// (`npm run dev:web`) there is no backend, so the clearly labelled sample data is shown instead.
// Release builds only ever run inside the app, so the dev-only check below lets the bundler
// leave the sample data out of them.
const source: SnapshotSource =
  import.meta.env.DEV && !isTauri() ? new SampleSnapshotSource({ liveUpdates: true }) : new PhoneSnapshotSource()

// With edge-to-edge the page runs under Android's navigation bar, and the WebView reports only the
// status bar to CSS. Ask Android for the bar's height and give it to the bottom tabs (`--nav-inset`).
function syncNavigationInset() {
  invoke<{ bottom: number }>('navigation_inset')
    .then(({ bottom }) => {
      if (Number.isFinite(bottom) && bottom >= 0 && bottom < 200) {
        document.documentElement.style.setProperty('--nav-inset', `${bottom}px`)
      }
    })
    .catch(() => {})
}
if (isTauri()) {
  syncNavigationInset()
  // Rotation or a switch between gesture and button navigation changes the bar.
  window.addEventListener('resize', syncNavigationInset)
}

const rootEl = document.getElementById('root')
if (!rootEl) throw new Error('Missing #root element')

createRoot(rootEl).render(
  <StrictMode>
    <App source={source} />
  </StrictMode>,
)

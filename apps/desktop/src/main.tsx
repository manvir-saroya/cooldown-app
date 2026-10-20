import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { isTauri } from '@tauri-apps/api/core'
import { App, SampleSnapshotSource, type SnapshotSource } from '@cooldown/ui'
import './index.css'
import { TauriSnapshotSource } from './TauriSnapshotSource'

// Inside the app window the Rust backend serves real data. Opened in a plain browser
// (`npm run dev:web`) there is no backend, so the clearly labelled sample data is shown instead.
// Release builds only ever run inside the app window, so the dev-only check below lets the bundler
// leave the sample data out of them.
const source: SnapshotSource =
  import.meta.env.DEV && !isTauri() ? new SampleSnapshotSource({ liveUpdates: true }) : new TauriSnapshotSource()

const rootEl = document.getElementById('root')
if (!rootEl) throw new Error('Missing #root element')

createRoot(rootEl).render(
  <StrictMode>
    <App source={source} />
  </StrictMode>,
)

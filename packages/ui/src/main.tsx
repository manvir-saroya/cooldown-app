import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import { App } from './App'
import { SampleSnapshotSource } from './data/SampleSnapshotSource'

// `?frozen` pins the clock and turns off the simulated adapter, for screenshots and demos that must not change.
const frozen = new URLSearchParams(location.search).has('frozen')
const fixedNow = frozen ? Date.now() : undefined
const source = new SampleSnapshotSource({ now: fixedNow === undefined ? undefined : () => fixedNow, liveUpdates: !frozen })

const rootEl = document.getElementById('root')
if (!rootEl) throw new Error('Missing #root element')

createRoot(rootEl).render(
  <StrictMode>
    <App source={source} fixedNow={fixedNow} initialArmed={['codex:personal:session_5h']} />
  </StrictMode>,
)

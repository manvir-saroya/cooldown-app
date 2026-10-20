/**
 * Public surface of @cooldown/ui, for the desktop window, the local browser tab and the Android shell.
 *
 *   import { App, SampleSnapshotSource } from '@cooldown/ui'
 *   import '@cooldown/ui/styles.css'
 *
 * Provide your own SnapshotSource (Tauri invoke or local HTTP) instead of the sample one.
 */
export { App, type AppProps } from './App'
export { SampleSnapshotSource, type SampleSnapshotSourceOptions } from './data/SampleSnapshotSource'
export {
  SnapshotSourceProvider,
  useSnapshotSource,
  useSnapshots,
  useSourceQuery,
  type AlertBackend,
  type PairingControl,
  type PhoneControl,
  type Query,
  type SnapshotSource,
  type NotificationControl,
  type WidgetControl,
} from './data/SnapshotSource'
export { CHANGED_EVENT, TauriBackedSource, type InvokeFn, type ListenFn } from './data/TauriBackedSource'
export type {
  AdapterInfo,
  HistoryPoint,
  HistoryRange,
  HistorySeries,
  HookStatus,
  PairedDevice,
  PairedPc,
  PairingPayload,
  PairingState,
  PhonePermissions,
  NotificationSettings,
  WidgetSettings,
} from './data/types'

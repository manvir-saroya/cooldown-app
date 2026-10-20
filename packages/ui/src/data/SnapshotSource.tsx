/**
 * The data-access seam for the whole dashboard.
 *
 * Components never fetch. They read from a SnapshotSource through the hooks in
 * this file. The implementations are SampleSnapshotSource (invented data for the
 * dev server and tests), TauriSnapshotSource (desktop app) and PhoneSnapshotSource
 * (Android app); no component knows which one it is talking to.
 *
 * Contract notes:
 *  - getSnapshots() must keep the last good value for a failing adapter and
 *    report status 'stale' | 'degraded' | 'unavailable'; it never omits it.
 *  - API keys go in, never come out: there is no method that returns one.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import type { AlertIntent, Snapshot } from '@cooldown/snapshot-schema'
import type {
  AdapterInfo,
  HistoryRange,
  HistorySeries,
  HookStatus,
  NotificationSettings,
  PairedDevice,
  PairedPc,
  PairingPayload,
  PairingState,
  PhonePermissions,
  WidgetSettings,
} from './types'
import { shareStructure } from './share-structure'

/** Reset alerts kept by the source itself, so they ring even with the window closed. */
export interface AlertBackend {
  list(): Promise<AlertIntent[]>
  /** Store or replace the alert with this id. */
  set(intent: AlertIntent): Promise<void>
  remove(id: string): Promise<void>
}

/** Live pairing progress, for sources that pair for real. The PC confirms the code. */
export interface PairingControl {
  getState(): Promise<PairingState>
  /** The user's answer to "Is this the code on your phone?". */
  confirm(accept: boolean): Promise<void>
  /** Hide the code: the one-time secret stops working. */
  cancel(): Promise<void>
  /**
   * Optional. Calls `listener` when the app window is closed (the app keeps running in the tray), so
   * a pairing code does not stay on a page nobody is looking at. Returns the function that stops listening.
   */
  onWindowHidden?(listener: () => void): () => void
}

/** The automatic notifications ("limit is back", "ran out", "running low") on this device. */
export interface NotificationControl {
  get(): Promise<NotificationSettings>
  set(settings: NotificationSettings): Promise<void>
}

/** The home-screen widget of the phone app: which windows it shows and putting it on the home screen. */
export interface WidgetControl {
  getSettings(): Promise<WidgetSettings>
  /** Choose the windows by snapshot id (at least one), or `null` for every window. Resolves with the new settings. */
  setWindows(windows: string[] | null): Promise<WidgetSettings>
  /** Asks the launcher to add the widget. Resolves false when the launcher cannot do that from inside an app. */
  pin(): Promise<boolean>
}

/**
 * Phone mode. A source that runs on a phone provides this, and the UI changes shape: pairing scans a
 * code from the PC, Settings shows the paired PC and Android permissions instead of keys and the
 * Claude hook, and bells are kept on the PC (arming one goes through `alerts`, the PC applies it and
 * syncs it back).
 */
export interface PhoneControl {
  /** Opens the camera and resolves with the text of the code it read. Rejects with 'cancelled' if the user backs out. */
  scanQr(): Promise<string>
  /** Starts pairing from a code's text. Resolves with the six digits the PC will show too. */
  beginPairing(qrText: string): Promise<{ code: string; pcName: string }>
  /** Resolves when the PC confirmed the code. Rejects with the reason if it did not. */
  finishPairing(): Promise<void>
  pairedPcs(): Promise<PairedPc[]>
  unpair(pcId: string): Promise<void>
  /** Try the PC again, for example after the Wi-Fi came back. */
  reconnect(): Promise<void>
  permissions(): Promise<PhonePermissions>
  /** Shows Android's prompts, or opens its settings page. Resolves with what is allowed afterwards. */
  askPermissions(): Promise<PhonePermissions>
  /** Optional: a phone with a home-screen widget. Without it Settings has no widget section. */
  readonly widget?: WidgetControl
}

export interface SnapshotSource {
  /** Short label shown in the UI so sample data is never mistaken for real data. */
  readonly label: string
  readonly isSample: boolean

  getSnapshots(): Promise<Snapshot[]>
  getHistory(snapshotId: string, range: HistoryRange): Promise<HistorySeries>

  getAdapters(): Promise<AdapterInfo[]>
  /** Stores a key in the OS credential store. Resolves with no value on purpose. */
  saveApiKey(adapterId: string, key: string, extra?: { teamId?: string }): Promise<void>
  removeApiKey(adapterId: string): Promise<void>
  /** Optional. Switch a source on or off; an off source is not read and not shown on the dashboard. */
  setSourceEnabled?(adapterId: string, enabled: boolean): Promise<void>
  /** Optional. Status line hooks for terminal agents other than Claude Code (`AdapterInfo.hookAgent`). */
  readonly agentHooks?: {
    status(agent: string): Promise<HookStatus>
    install(agent: string): Promise<void>
    uninstall(agent: string): Promise<void>
  }
  /** Optional. The Copilot plan's monthly AI-credit allowance in USD, which GitHub's API does not report. */
  getCopilotAllowance?(): Promise<number | null>
  setCopilotAllowance?(usd: number | null): Promise<void>

  getHookStatus(): Promise<HookStatus>
  installHook(): Promise<void>
  uninstallHook(): Promise<void>

  getPairedDevices(): Promise<PairedDevice[]>
  revokeDevice(deviceId: string): Promise<void>
  createPairingPayload(): Promise<PairingPayload>

  /** Ask adapters to poll now. Resolves when the new snapshots are available. */
  refresh(): Promise<void>
  /** Called whenever any data the source serves may have changed. Returns an unsubscribe function. */
  subscribe(listener: () => void): () => void

  /** Optional. When present, alerts are stored and rung by the source; otherwise they live in the UI. */
  readonly alerts?: AlertBackend
  /** Optional. When present, the Pairing screen shows progress and asks for the PC-side confirmation. */
  readonly pairing?: PairingControl
  /** Optional. When present the UI is in phone mode (see PhoneControl). */
  readonly phone?: PhoneControl
  /** Optional. When present, Settings shows the automatic notifications. */
  readonly notifications?: NotificationControl
}

const SourceContext = createContext<SnapshotSource | null>(null)

const SnapshotsContext = createContext<Query<Snapshot[]> | null>(null)

export function SnapshotSourceProvider({
  source,
  children,
}: {
  source: SnapshotSource
  children: ReactNode
}) {
  return (
    <SourceContext.Provider value={source}>
      <SnapshotsStore>{children}</SnapshotsStore>
    </SourceContext.Provider>
  )
}

/** Fetches the snapshot list once and shares it, so every screen and the alert reconciler see the same data. */
function SnapshotsStore({ children }: { children: ReactNode }) {
  const query = useSourceQuery((s) => s.getSnapshots())
  return <SnapshotsContext.Provider value={query}>{children}</SnapshotsContext.Provider>
}

export function useSnapshotSource(): SnapshotSource {
  const source = useContext(SourceContext)
  if (!source) throw new Error('useSnapshotSource must be used inside <SnapshotSourceProvider>')
  return source
}

export type Query<T> = {
  data: T | undefined
  error: Error | undefined
  /** True only before the first result. Later refetches keep showing the last data. */
  loading: boolean
  reload: () => void
}

/**
 * Runs `fetcher` against the source, again whenever the source reports a change
 * or `deps` change, and keeps the previous data visible while refetching.
 */
export function useSourceQuery<T>(
  fetcher: (source: SnapshotSource) => Promise<T>,
  deps: readonly unknown[] = [],
): Query<T> {
  const source = useSnapshotSource()
  const [state, setState] = useState<{ data?: T; error?: Error; loading: boolean }>({ loading: true })
  const [tick, setTick] = useState(0)
  const fetcherRef = useRef(fetcher)
  fetcherRef.current = fetcher

  useEffect(() => source.subscribe(() => setTick((n) => n + 1)), [source])

  useEffect(() => {
    let cancelled = false
    fetcherRef.current(source).then(
      (data) => {
        if (cancelled) return
        // A refetch that brings the same content keeps the same object: nothing downstream renders again.
        setState((prev) => {
          const shared = shareStructure(prev.data, data)
          return prev.data === shared && !prev.loading && prev.error === undefined ? prev : { data: shared, loading: false }
        })
      },
      (err: unknown) => {
        if (!cancelled) {
          setState((prev) => ({
            data: prev.data,
            error: err instanceof Error ? err : new Error(String(err)),
            loading: false,
          }))
        }
      },
    )
    return () => {
      cancelled = true
    }
    // `deps` is the caller's list of inputs to the fetcher; the fetcher itself is read through a ref.
  }, [source, tick, ...deps])

  const reload = useCallback(() => setTick((n) => n + 1), [])
  return useMemo(
    () => ({ data: state.data, error: state.error, loading: state.loading, reload }),
    [state, reload],
  )
}

export function useSnapshots(): Query<Snapshot[]> {
  const query = useContext(SnapshotsContext)
  if (!query) throw new Error('useSnapshots must be used inside <SnapshotSourceProvider>')
  return query
}

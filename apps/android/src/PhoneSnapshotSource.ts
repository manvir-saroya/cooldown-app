/**
 * The SnapshotSource backed by the Rust side of the phone app (`src-tauri/src/lib.rs`).
 *
 * On a phone the PC is the one that reads providers and keeps keys and bells; the phone gets
 * sanitized readings and bell times over the sync link. So reads are `invoke`s, bell changes are
 * requests to the PC (it applies them and syncs back), and everything that is about keys, the Claude
 * hook or history says so kindly instead of pretending. The backend emits `cooldown://changed` when a
 * sync arrives or anything else changed; it means "refetch".
 */
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { AlertIntent, Snapshot } from '@cooldown/snapshot-schema'
import {
  CHANGED_EVENT,
  TauriBackedSource,
  type AdapterInfo,
  type AlertBackend,
  type HistoryRange,
  type HistorySeries,
  type HookStatus,
  type InvokeFn,
  type ListenFn,
  type NotificationControl,
  type NotificationSettings,
  type PairedDevice,
  type PairedPc,
  type PairingPayload,
  type PhoneControl,
  type PhonePermissions,
  type SnapshotSource,
  type WidgetSettings,
} from '@cooldown/ui'

export { CHANGED_EVENT, type InvokeFn, type ListenFn }

/** Times arrive as ISO-8601 strings; accept epoch milliseconds too so a backend change cannot blank a screen. */
function toIso(value: unknown): string | undefined {
  if (typeof value === 'string') return value
  if (typeof value === 'number' && Number.isFinite(value)) return new Date(value).toISOString()
  return undefined
}

function toPermissions(raw: Partial<PhonePermissions> | null | undefined): PhonePermissions {
  return {
    notifications: raw?.notifications === true,
    exactAlarms: raw?.exactAlarms === true,
    localNetwork: raw?.localNetwork === true,
  }
}

function toWidgetSettings(raw: Partial<WidgetSettings> | null | undefined): WidgetSettings {
  const windows = Array.isArray(raw?.windows) ? raw.windows.filter((id): id is string => typeof id === 'string') : null
  return { windows: windows && windows.length > 0 ? windows : null, canPin: raw?.canPin === true }
}

/** Keys, the hook and the pairing offer all belong to the PC. */
const ON_THE_PC = {
  keys: 'Keys stay on your PC. Add or change them in Cooldown on your PC.',
  hook: 'The Claude Code hook lives on your PC. Set it up in Cooldown there.',
  pairingOffer: 'This phone scans a code from your PC. Open Pair a device in Cooldown on the PC.',
  devices: 'This phone follows your PC. Manage paired phones in Cooldown on the PC.',
} as const

export class PhoneSnapshotSource extends TauriBackedSource implements SnapshotSource {
  readonly label = 'Your PC'
  readonly isSample = false
  readonly alerts: AlertBackend
  readonly phone: PhoneControl
  readonly notifications: NotificationControl

  constructor(
    invokeFn: InvokeFn = invoke,
    listenFn: ListenFn = (event, handler) => listen(event, handler),
  ) {
    super(invokeFn, listenFn)
    // A bell change is a request to the PC. It applies it and syncs back, which fires CHANGED_EVENT, so
    // nothing is emitted here: refetching before the PC answered would put the old state back on screen.
    this.alerts = {
      list: () => this.call<AlertIntent[]>('get_alerts'),
      set: (intent) => this.call<void>('set_alert', { id: intent.id, enabled: intent.enabled }),
      // The PC owns the bell: "remove" pauses it there. It comes back in the list as disabled.
      remove: (id) => this.call<void>('set_alert', { id, enabled: false }),
    }
    this.phone = {
      scanQr: () => this.call<string>('scan_qr'),
      beginPairing: (qrText) => this.call<{ code: string; pcName: string }>('pair_begin', { qr: qrText }),
      finishPairing: () => this.mutate('pair_finish'),
      pairedPcs: async () => {
        const list = await this.call<PairedPc[] | null>('paired_pcs')
        return (list ?? []).map((pc) => ({
          id: pc.id,
          name: pc.name,
          pairedAt: toIso(pc.pairedAt) ?? new Date(0).toISOString(),
          lastSeenAt: toIso(pc.lastSeenAt),
          connected: pc.connected === true,
        }))
      },
      unpair: (pcId) => this.mutate('unpair', { pcId }),
      reconnect: () => this.mutate('reconnect'),
      permissions: async () => toPermissions(await this.call<PhonePermissions>('alarm_permissions')),
      askPermissions: async () => toPermissions(await this.call<PhonePermissions>('request_permissions')),
      widget: {
        getSettings: async () => toWidgetSettings(await this.call<WidgetSettings>('widget_config')),
        setWindows: async (windows) => toWidgetSettings(await this.call<WidgetSettings>('set_widget_config', { windows })),
        pin: async () => (await this.call<{ requested?: boolean }>('pin_widget'))?.requested === true,
      },
    }
    // "Limit is back" bells come from the PC, so the phone only has the other two.
    this.notifications = {
      get: async () => {
        const raw = await this.call<Partial<NotificationSettings> | null>('get_notifications')
        return { ranOut: raw?.ranOut !== false, lowAtPercent: raw?.lowAtPercent ?? null }
      },
      set: (settings) =>
        this.call<void>('set_notifications', {
          notifications: { ranOut: settings.ranOut, lowAtPercent: settings.lowAtPercent },
        }),
    }
  }

  getSnapshots(): Promise<Snapshot[]> {
    return this.call('get_snapshots')
  }

  /**
   * The phone keeps no history: it only holds the latest sync. The series is the current reading as a
   * single point, which is honest; the UI says history lives on the PC instead of drawing it.
   */
  async getHistory(snapshotId: string, range: HistoryRange): Promise<HistorySeries> {
    const snapshots = await this.getSnapshots()
    const snapshot = snapshots.find((s) => s.id === snapshotId)
    if (!snapshot) throw new Error('That reading is not on this phone any more.')
    const balance = snapshot.window === 'balance' || snapshot.unit === 'usd' || snapshot.unit === 'cny'
    const percent =
      snapshot.percentUsed ?? (snapshot.used !== undefined && snapshot.limit ? (snapshot.used / snapshot.limit) * 100 : undefined)
    const value = balance ? snapshot.remaining : percent
    const t = Date.parse(snapshot.observedAt)
    return {
      snapshotId,
      unit: snapshot.unit,
      kind: balance ? 'balance' : 'usage',
      range,
      points: value === undefined || Number.isNaN(t) ? [] : [{ t, value }],
      resets: [],
    }
  }

  /** Adapters run on the PC. */
  async getAdapters(): Promise<AdapterInfo[]> {
    return []
  }

  saveApiKey(_adapterId: string, _key: string, _extra?: { teamId?: string }): Promise<void> {
    return Promise.reject(new Error(ON_THE_PC.keys))
  }

  removeApiKey(_adapterId: string): Promise<void> {
    return Promise.reject(new Error(ON_THE_PC.keys))
  }

  getHookStatus(): Promise<HookStatus> {
    return Promise.reject(new Error(ON_THE_PC.hook))
  }

  installHook(): Promise<void> {
    return Promise.reject(new Error(ON_THE_PC.hook))
  }

  uninstallHook(): Promise<void> {
    return Promise.reject(new Error(ON_THE_PC.hook))
  }

  /** The list of paired phones is the PC's. This phone's own pairing is `phone.pairedPcs()`. */
  async getPairedDevices(): Promise<PairedDevice[]> {
    return []
  }

  revokeDevice(_deviceId: string): Promise<void> {
    return Promise.reject(new Error(ON_THE_PC.devices))
  }

  createPairingPayload(): Promise<PairingPayload> {
    return Promise.reject(new Error(ON_THE_PC.pairingOffer))
  }

  /** "Refresh" on a phone means asking the PC again. */
  refresh(): Promise<void> {
    return this.mutate('reconnect')
  }
}

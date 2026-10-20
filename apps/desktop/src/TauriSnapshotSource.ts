/**
 * The SnapshotSource backed by the Rust side of the desktop app (`src-tauri/src/commands.rs`).
 *
 * Every method is one `invoke`. The backend emits `cooldown://changed` whenever a reading, an
 * alert or a paired phone changes; subscribers are told and refetch. API keys go in through
 * `saveApiKey` and never come back out: no command returns one.
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
  type PairingControl,
  type PairingPayload,
  type PairingState,
  type SnapshotSource,
} from '@cooldown/ui'

export { CHANGED_EVENT, type InvokeFn, type ListenFn }
/** Sent when the window is closed to the tray, so a showing pairing code can be dropped. */
export const WINDOW_HIDDEN_EVENT = 'cooldown://window-hidden'

export class TauriSnapshotSource extends TauriBackedSource implements SnapshotSource {
  readonly label = 'This PC'
  readonly isSample = false
  readonly alerts: AlertBackend
  readonly pairing: PairingControl
  readonly notifications: NotificationControl
  readonly agentHooks = {
    status: (agent: string) => this.call<HookStatus>('get_agent_hook_status', { agent }),
    install: (agent: string) => this.mutate('install_agent_hook', { agent }),
    uninstall: (agent: string) => this.mutate('uninstall_agent_hook', { agent }),
  }

  constructor(
    invokeFn: InvokeFn = invoke,
    listenFn: ListenFn = (event, handler) => listen(event, handler),
  ) {
    super(invokeFn, listenFn)
    this.alerts = {
      list: () => this.call<AlertIntent[]>('get_alerts'),
      set: (intent) => this.call<void>('set_alert', { intent }),
      remove: (id) => this.call<void>('remove_alert', { id }),
    }
    this.pairing = {
      getState: () => this.call<PairingState>('get_pairing_state'),
      confirm: (accept) => this.mutate('confirm_pairing', { accept }),
      cancel: () => this.call<void>('cancel_pairing'),
      onWindowHidden: (listener) => {
        const pending = this.listenFn(WINDOW_HIDDEN_EVENT, listener)
        return () => void pending.then((stop) => stop(), () => {})
      },
    }
    this.notifications = {
      get: () => this.call<NotificationSettings>('get_notifications'),
      set: (notifications) =>
        this.mutate('set_notifications', {
          notifications: {
            limitBack: notifications.limitBack ?? true,
            ranOut: notifications.ranOut,
            lowAtPercent: notifications.lowAtPercent,
          },
        }),
    }
  }

  getSnapshots(): Promise<Snapshot[]> {
    return this.call('get_snapshots')
  }

  getHistory(snapshotId: string, range: HistoryRange): Promise<HistorySeries> {
    return this.call('get_history', { snapshotId, range })
  }

  getAdapters(): Promise<AdapterInfo[]> {
    return this.call('get_adapters')
  }

  async saveApiKey(adapterId: string, key: string, extra?: { teamId?: string }): Promise<void> {
    if (key.trim().length === 0) throw new Error('Enter a key first.')
    await this.mutate('save_api_key', { adapterId, key, teamId: extra?.teamId ?? null })
  }

  removeApiKey(adapterId: string): Promise<void> {
    return this.mutate('remove_api_key', { adapterId })
  }

  setSourceEnabled(adapterId: string, enabled: boolean): Promise<void> {
    return this.mutate('set_source_enabled', { adapterId, enabled })
  }

  async getCopilotAllowance(): Promise<number | null> {
    return (await this.call<number | null>('get_copilot_allowance')) ?? null
  }

  setCopilotAllowance(usd: number | null): Promise<void> {
    return this.mutate('set_copilot_allowance', { usd })
  }

  getHookStatus(): Promise<HookStatus> {
    return this.call('get_hook_status')
  }

  installHook(): Promise<void> {
    return this.mutate('install_hook')
  }

  uninstallHook(): Promise<void> {
    return this.mutate('uninstall_hook')
  }

  getPairedDevices(): Promise<PairedDevice[]> {
    return this.call('get_paired_devices')
  }

  revokeDevice(deviceId: string): Promise<void> {
    return this.mutate('revoke_device', { deviceId })
  }

  createPairingPayload(): Promise<PairingPayload> {
    return this.call('create_pairing_payload')
  }

  async refresh(): Promise<void> {
    await this.call<void>('refresh')
    this.emit()
  }
}

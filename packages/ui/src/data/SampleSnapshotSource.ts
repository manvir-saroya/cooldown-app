import type { Snapshot } from '@cooldown/snapshot-schema'
import type { NotificationControl, SnapshotSource } from './SnapshotSource'
import { buildSampleHistory } from './sample-history'
import {
  buildSampleAdapters,
  buildSampleDevices,
  buildSampleHook,
  buildSampleSnapshots,
} from './sample-snapshots'
import type {
  AdapterInfo,
  HistoryRange,
  HistorySeries,
  HookStatus,
  NotificationSettings,
  PairedDevice,
  PairingPayload,
} from './types'

export interface SampleSnapshotSourceOptions {
  /** Clock. Inject a fixed one in tests and screenshots. */
  now?: () => number
  /** Artificial delay for every call, to exercise loading states. */
  latencyMs?: number
  /**
   * Simulate an adapter noticing a reset a few seconds after it happens and
   * publishing a fresh window, so the "limit is back" moment can be seen live.
   */
  liveUpdates?: boolean
}

const WINDOW_5H = 5 * 3_600_000

/** Realistic fake data behind the SnapshotSource seam. Nothing here touches the network or disk. */
export class SampleSnapshotSource implements SnapshotSource {
  readonly label = 'Sample data'
  readonly isSample = true

  private readonly clock: () => number
  private readonly latencyMs: number
  private snapshots: Snapshot[]
  private adapters: AdapterInfo[]
  private devices: PairedDevice[]
  private hook: HookStatus
  private pairingCount = 0
  private notificationSettings: NotificationSettings = { limitBack: true, ranOut: true, lowAtPercent: 90 }
  private readonly listeners = new Set<() => void>()
  private readonly timers = new Set<ReturnType<typeof setTimeout>>()

  readonly notifications: NotificationControl = {
    get: () => this.delay({ ...this.notificationSettings }),
    set: async (settings) => {
      const low = settings.lowAtPercent
      if (low !== null && !(low >= 1 && low <= 99)) throw new Error('The warning level must be between 1% and 99%.')
      this.notificationSettings = { ...settings }
      await this.delay(undefined)
      this.emit()
    },
  }

  constructor(options: SampleSnapshotSourceOptions = {}) {
    this.clock = options.now ?? (() => Date.now())
    this.latencyMs = options.latencyMs ?? 0
    const now = this.clock()
    this.snapshots = buildSampleSnapshots(now)
    this.adapters = buildSampleAdapters(now)
    this.devices = buildSampleDevices(now)
    this.hook = buildSampleHook(now)
    if (options.liveUpdates) this.scheduleResetPickup()
  }

  /** Stop any timers. Call when the source is replaced (tests, hot reload). */
  dispose(): void {
    for (const t of this.timers) clearTimeout(t)
    this.timers.clear()
    this.listeners.clear()
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  private emit(): void {
    for (const l of [...this.listeners]) l()
  }

  private async delay<T>(value: T): Promise<T> {
    if (this.latencyMs > 0) await new Promise((r) => setTimeout(r, this.latencyMs))
    return value
  }

  getSnapshots(): Promise<Snapshot[]> {
    const off = new Set(this.adapters.filter((a) => a.enabled === false).map((a) => a.provider))
    return this.delay(this.snapshots.filter((s) => !off.has(s.provider)).map((s) => ({ ...s, source: { ...s.source } })))
  }

  private agentHookInstalled = new Set<string>()

  readonly agentHooks = {
    status: async (agent: string): Promise<HookStatus> =>
      this.delay({
        installed: this.agentHookInstalled.has(agent),
        chainsExisting: false,
        settingsPath: '~/.gemini/antigravity-cli/settings.json',
        inboxPath: '~/.cooldown/inbox/antigravity-statusline.json',
      }),
    install: async (agent: string) => {
      this.agentHookInstalled.add(agent)
      await this.delay(undefined)
      this.emit()
    },
    uninstall: async (agent: string) => {
      this.agentHookInstalled.delete(agent)
      await this.delay(undefined)
      this.emit()
    },
  }

  private copilotAllowance: number | null = null

  getCopilotAllowance(): Promise<number | null> {
    return this.delay(this.copilotAllowance)
  }

  async setCopilotAllowance(usd: number | null): Promise<void> {
    if (usd !== null && !(usd > 0 && usd <= 100_000)) throw new Error('Enter the allowance in dollars, for example 10 or 39.')
    this.copilotAllowance = usd
    await this.delay(undefined)
    this.emit()
  }

  async setSourceEnabled(adapterId: string, enabled: boolean): Promise<void> {
    if (!this.adapters.some((a) => a.id === adapterId)) throw new Error('Unknown source.')
    // Same wording as the desktop backend: an off source says so, a source just switched on waits for its first reading.
    this.adapters = this.adapters.map((a) =>
      a.id !== adapterId
        ? a
        : enabled
          ? { ...a, enabled, status: a.lastPolledAt ? a.status : 'stale', note: a.lastPolledAt ? a.note : 'Waiting for the first reading.' }
          : { ...a, enabled, status: 'unavailable', note: 'Switched off. It is not read and not shown.' },
    )
    await this.delay(undefined)
    this.emit()
  }

  getHistory(snapshotId: string, range: HistoryRange): Promise<HistorySeries> {
    const snapshot = this.snapshots.find((s) => s.id === snapshotId)
    if (!snapshot) return Promise.reject(new Error(`No snapshot with id ${snapshotId}`))
    return this.delay(buildSampleHistory(snapshot, range, this.clock()))
  }

  getAdapters(): Promise<AdapterInfo[]> {
    return this.delay(this.adapters.map((a) => ({ ...a })))
  }

  async saveApiKey(adapterId: string, key: string, _extra?: { teamId?: string }): Promise<void> {
    if (key.trim().length === 0) throw new Error('Enter a key first.')
    // Sample source: the key is validated for presence only and is not kept anywhere.
    this.adapters = this.adapters.map((a) =>
      a.id === adapterId ? { ...a, hasKey: true, status: 'ok', note: undefined, lastPolledAt: new Date(this.clock()).toISOString() } : a,
    )
    await this.delay(undefined)
    this.emit()
  }

  async removeApiKey(adapterId: string): Promise<void> {
    this.adapters = this.adapters.map((a) => (a.id === adapterId ? { ...a, hasKey: false } : a))
    await this.delay(undefined)
    this.emit()
  }

  getHookStatus(): Promise<HookStatus> {
    return this.delay({ ...this.hook })
  }

  async installHook(): Promise<void> {
    this.hook = { ...buildSampleHook(this.clock()), installed: true, lastDataAt: undefined }
    await this.delay(undefined)
    this.emit()
  }

  async uninstallHook(): Promise<void> {
    this.hook = { ...this.hook, installed: false, chainsExisting: false, backupPath: undefined, lastDataAt: undefined }
    await this.delay(undefined)
    this.emit()
  }

  getPairedDevices(): Promise<PairedDevice[]> {
    return this.delay(this.devices.map((d) => ({ ...d })))
  }

  async revokeDevice(deviceId: string): Promise<void> {
    this.devices = this.devices.filter((d) => d.id !== deviceId)
    await this.delay(undefined)
    this.emit()
  }

  createPairingPayload(): Promise<PairingPayload> {
    this.pairingCount += 1
    const n = this.pairingCount
    const host = '192.168.1.24'
    const port = 47821
    // Placeholder text in the real shape: the `CD1:` prefix and RFC 4648 base32 (A-Z, 2-7), though
    // far shorter. A real payload is the same text for the QR code and for the typed code (the
    // phone's "Paste code instead"), and a secret until pairing ends.
    const groups = ['K4MX', 'QDNT', 'TNAH', 'PWBZ', 'CMRE', 'JFGY']
    const a = groups[n % groups.length]
    const b = groups[(n + 2) % groups.length]
    const c = groups[(n + 4) % groups.length]
    const code = `CD1:${a}${b}${c}`
    return this.delay({
      uri: code,
      manualCode: code,
      host,
      port,
      expiresAt: new Date(this.clock() + 2 * 60_000).toISOString(),
      fingerprint: ['4F2A 91C7', 'A07D 5E12', '93BC 20F8'][n % 3] ?? '4F2A 91C7',
      placeholder: true,
    })
  }

  async refresh(): Promise<void> {
    const now = new Date(this.clock()).toISOString()
    // Adapters that are running re-observe. Stale and unavailable ones cannot.
    this.snapshots = this.snapshots.map((s) => (s.status === 'ok' && s.source.kind !== 'manual' ? { ...s, observedAt: now } : s))
    await this.delay(undefined)
    this.emit()
  }

  /** After the soonest reset passes, publish a new empty window, like a real adapter would. */
  private scheduleResetPickup(): void {
    const now = this.clock()
    for (const s of this.snapshots) {
      const resetsAt = Date.parse(s.resetsAt ?? '')
      if (s.status !== 'ok' || s.window !== 'session_5h' || !Number.isFinite(resetsAt) || resetsAt <= now) continue
      const wait = resetsAt - now + 6_000
      if (wait > 30 * 60_000) continue
      const timer = setTimeout(() => {
        this.timers.delete(timer)
        const at = this.clock()
        this.snapshots = this.snapshots.map((x) =>
          x.id === s.id
            ? {
                ...x,
                percentUsed: 0,
                resetsAt: new Date(resetsAt + WINDOW_5H).toISOString(),
                observedAt: new Date(at).toISOString(),
              }
            : x,
        )
        this.emit()
      }, wait)
      this.timers.add(timer)
    }
  }
}

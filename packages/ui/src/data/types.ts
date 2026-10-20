/**
 * View-side types that sit next to the frozen Snapshot contract.
 * Nothing here is part of the sync wire format; it is what the dashboard
 * screens need that a Snapshot alone does not carry.
 */
import type { SourceKind, Status, Unit } from '@cooldown/snapshot-schema'

export type HistoryRange = '24h' | '7d' | '30d'

export const HISTORY_RANGES: { value: HistoryRange; label: string; ms: number }[] = [
  { value: '24h', label: '24 hours', ms: 24 * 3600 * 1000 },
  { value: '7d', label: '7 days', ms: 7 * 24 * 3600 * 1000 },
  { value: '30d', label: '30 days', ms: 30 * 24 * 3600 * 1000 },
]

export interface HistoryPoint {
  /** Epoch milliseconds. */
  t: number
  /** Percent used (0..100) for percent windows, remaining USD for balances. */
  value: number
}

export interface HistorySeries {
  snapshotId: string
  unit: Unit
  /** 'usage' climbs and resets; 'balance' falls and is topped up. */
  kind: 'usage' | 'balance'
  range: HistoryRange
  points: HistoryPoint[]
  /** Epoch ms of every window reset seen inside the range (usage only). */
  resets: number[]
}

export interface AdapterInfo {
  id: string
  name: string
  provider: string
  kind: SourceKind
  version: string
  status: Status
  /** Reason when the status is not ok. */
  note?: string
  /** ISO-8601 UTC of the last poll attempt. */
  lastPolledAt?: string
  /** What the adapter needs from the user, if anything. */
  credentials: 'none' | 'api-key' | 'management-key' | 'github-token'
  /** True once a key is stored in the OS credential store. The key itself never reaches the UI. */
  hasKey: boolean
  /**
   * Whether the user has this source switched on: read and shown on the dashboard. Absent means
   * on (sources that cannot be switched off).
   */
  enabled?: boolean
  /** Optional one-line setup hint shown under the name (for example how to connect a terminal agent). */
  setup?: string
  /** The terminal agent whose status line hook feeds this source (other than Claude Code), e.g. 'antigravity'. */
  hookAgent?: string
}

export interface PairedDevice {
  id: string
  name: string
  platform: 'android' | 'windows'
  /** ISO-8601 UTC */
  pairedAt: string
  /** ISO-8601 UTC */
  lastSeenAt?: string
}

/** A PC this phone is paired with. Phone mode only: the PC is the one that reads providers and keeps the bells. */
export interface PairedPc {
  id: string
  name: string
  /** ISO-8601 UTC */
  pairedAt: string
  /** ISO-8601 UTC. When the PC last answered. */
  lastSeenAt?: string
  /** True while this phone has a live connection to the PC. */
  connected: boolean
}

/** What Android lets the phone app do. Each missing one has a consequence the UI explains. */
export interface PhonePermissions {
  /** A bell can show up on screen. */
  notifications: boolean
  /** A bell rings at the exact reset time instead of whenever Android gets round to it. */
  exactAlarms: boolean
  /** The phone may talk to devices on the Wi-Fi (Android 17 asks first). Needed to reach the PC. */
  localNetwork: boolean
}

/** Automatic notifications. Every one is on by default. */
export interface NotificationSettings {
  /**
   * Ring when a used-up limit resets, without pressing its bell. Absent on a phone: its bells come
   * from the PC, so this is set there.
   */
  limitBack?: boolean
  /** Ring when a limit reaches 100%. */
  ranOut: boolean
  /** Ring once per cycle when a limit passes this percentage (1 to 99). `null` is off. */
  lowAtPercent: number | null
}

/** What the Android home-screen widget shows, and whether the app can place it. */
export interface WidgetSettings {
  /** Snapshot ids of the windows on the widget, or `null` for every window (the default). */
  windows: string[] | null
  /** True when the launcher can add the widget from inside the app. */
  canPin: boolean
}

export interface HookStatus {
  installed: boolean
  /** True when an existing statusline command was backed up and is still called. */
  chainsExisting: boolean
  settingsPath: string
  backupPath?: string
  inboxPath: string
  /** ISO-8601 UTC of the last write the hook made to the inbox. */
  lastDataAt?: string
}

export interface PairingPayload {
  /** What the QR code encodes. */
  uri: string
  /**
   * The same text as `uri`, for a person to copy to the phone ("Paste code instead") when its camera
   * cannot scan. Empty hides the "Cannot scan?" section. A secret until pairing ends: never log it.
   */
  manualCode: string
  host: string
  port: number
  /** ISO-8601 UTC. Pairing secrets are one-time and short lived. */
  expiresAt: string
  /** Short comparison string both devices show so the user can confirm they match. */
  fingerprint: string
  /** True while this is a placeholder rather than a real handshake offer. */
  placeholder: boolean
}

/** Where a pairing stands on the PC (sources that pair for real). */
export type PairingState =
  | { phase: 'idle' }
  | { phase: 'waiting' }
  /** A phone finished the handshake: compare the code, then confirm on this PC. */
  | { phase: 'confirm'; name: string; platform: string; code: string }
  | { phase: 'paired'; name: string }
  | { phase: 'failed'; reason: string }

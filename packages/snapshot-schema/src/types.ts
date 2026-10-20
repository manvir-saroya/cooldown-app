// Frozen contract — see docs/superpowers/specs/2026-10-10-cooldown-design.md §4.
// Changing a shape here means bumping schemaVersion and updating every consumer.

export type SourceKind = 'official' | 'estimated' | 'scraped' | 'manual';
export type WindowKind = 'session_5h' | 'weekly' | 'monthly' | 'daily' | 'balance';
export type Status = 'ok' | 'stale' | 'degraded' | 'unavailable';
export type Unit = 'percent' | 'usd' | 'cny' | 'tokens' | 'requests';

export interface SnapshotSource {
  kind: SourceKind;
  adapter: string;
  adapterVersion: string;
}

export interface Snapshot {
  schemaVersion: 1;
  /** `${provider}:${account}:${window}` — stable, also used for alert IDs. */
  id: string;
  /** 'claude' | 'codex' | 'openrouter' | 'deepseek' | 'xai' | ... */
  provider: string;
  /** Local label chosen by the user. Never a token or an email address. */
  account: string;
  window: WindowKind;
  unit: Unit;
  /** 0..100 */
  percentUsed?: number;
  used?: number;
  limit?: number;
  remaining?: number;
  /** ISO-8601 UTC */
  resetsAt?: string;
  /** ISO-8601 UTC — when the adapter last saw this value. */
  observedAt: string;
  source: SnapshotSource;
  status: Status;
  /** Human-readable reason when status is not 'ok'. */
  note?: string;
}

export interface AlertIntent {
  /** `reset:${provider}:${account}:${window}` */
  id: string;
  /** ISO-8601 UTC */
  fireAt: string;
  enabled: boolean;
}

export interface SyncPayload {
  schemaVersion: 1;
  deviceId: string;
  /** ISO-8601 UTC */
  generatedAt: string;
  snapshots: Snapshot[];
  alerts: AlertIntent[];
}

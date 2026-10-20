// Pure helpers shared by every consumer (UI, notifier, sync). Every function that depends on the
// current time takes `now: Date` explicitly; nothing here reads the clock, the locale or the
// time zone. All arithmetic is on epoch milliseconds, so results are identical in every time zone
// and across DST changes.

import type { Snapshot, Status, WindowKind } from './types';

/** Default age after which an observation is considered stale: 30 minutes. */
export const DEFAULT_MAX_AGE_MS = 30 * 60 * 1000;

export interface EffectiveStatusOptions {
  /**
   * Maximum age of `observedAt` before the snapshot counts as stale, in milliseconds. `Infinity`
   * disables the age check. Invalid values (NaN, zero, negative) fall back to the default.
   */
  maxAgeMs?: number;
}

/** Higher is worse. A derived status may never be better than the stored one. */
const STATUS_SEVERITY: Record<Status, number> = { ok: 0, stale: 1, degraded: 2, unavailable: 3 };

function parseTime(iso: string | undefined): number {
  return iso === undefined ? Number.NaN : Date.parse(iso);
}

/**
 * The status a consumer should display right now.
 *
 * - A snapshot whose `resetsAt` is at or before `now` is 'stale' (spec section 4).
 * - A snapshot observed more than `maxAgeMs` before `now` is 'stale'. A future `observedAt`
 *   (clock skew) counts as age zero. An unparseable `observedAt` counts as stale.
 * - 'degraded' and 'unavailable' are never upgraded: the result is the worse of the stored status
 *   and the time-derived one.
 */
export function effectiveStatus(
  snapshot: Pick<Snapshot, 'status' | 'resetsAt' | 'observedAt'>,
  now: Date,
  options: EffectiveStatusOptions = {},
): Status {
  const nowMs = now.getTime();
  const maxAge =
    typeof options.maxAgeMs === 'number' && !Number.isNaN(options.maxAgeMs) && options.maxAgeMs > 0
      ? options.maxAgeMs
      : DEFAULT_MAX_AGE_MS;

  let derived: Status = 'ok';

  const resetsAtMs = parseTime(snapshot.resetsAt);
  if (!Number.isNaN(resetsAtMs) && resetsAtMs <= nowMs) derived = 'stale';

  const observedAtMs = parseTime(snapshot.observedAt);
  if (Number.isNaN(observedAtMs) || nowMs - observedAtMs > maxAge) derived = 'stale';

  return STATUS_SEVERITY[derived] > STATUS_SEVERITY[snapshot.status] ? derived : snapshot.status;
}

/** `${provider}:${account}:${window}` */
export function snapshotIdFor(parts: Pick<Snapshot, 'provider' | 'account' | 'window'>): string {
  return `${parts.provider}:${parts.account}:${parts.window}`;
}

/** `reset:${provider}:${account}:${window}`: the stable id of the reset alert for a snapshot. */
export function alertIdFor(parts: Pick<Snapshot, 'provider' | 'account' | 'window'>): string {
  return `reset:${parts.provider}:${parts.account}:${parts.window}`;
}

const NOMINAL_WINDOW_MINUTES: ReadonlyArray<readonly [WindowKind, number]> = [
  ['session_5h', 300],
  ['daily', 1440],
  ['weekly', 10080],
  ['monthly', 43200],
];

/**
 * Classify a window by its DURATION, never by which slot a provider put it in (Codex has been seen
 * returning the weekly window in its 'primary' slot). Matches within a relative tolerance
 * (default 10%, which also covers 28 to 31 day months). Returns undefined when nothing matches or
 * the input is not a positive finite number. Never returns 'balance'.
 */
export function windowFromDurationMinutes(
  minutes: number,
  tolerance = 0.1,
): Exclude<WindowKind, 'balance'> | undefined {
  if (typeof minutes !== 'number' || !Number.isFinite(minutes) || minutes <= 0) return undefined;
  let best: { kind: Exclude<WindowKind, 'balance'>; distance: number } | undefined;
  for (const [kind, nominal] of NOMINAL_WINDOW_MINUTES) {
    const distance = Math.abs(minutes - nominal) / nominal;
    if (distance <= tolerance && (best === undefined || distance < best.distance)) {
      best = { kind: kind as Exclude<WindowKind, 'balance'>, distance };
    }
  }
  return best?.kind;
}

/**
 * Whole seconds from `now` until `resetsAt`, rounded up, never negative. Undefined when the
 * snapshot has no (parseable) reset time.
 */
export function secondsUntilReset(snapshot: Pick<Snapshot, 'resetsAt'>, now: Date): number | undefined {
  const resetsAtMs = parseTime(snapshot.resetsAt);
  if (Number.isNaN(resetsAtMs)) return undefined;
  return Math.max(0, Math.ceil((resetsAtMs - now.getTime()) / 1000));
}

/**
 * Compact relative age: 'just now' (under a minute, or a future timestamp), '2 min ago',
 * '3 h ago', '2 d ago'. Truncates, so '119 s' is '1 min ago'. Unparseable input gives 'unknown'.
 */
export function formatAge(observedAt: string | Date, now: Date): string {
  const observedMs = observedAt instanceof Date ? observedAt.getTime() : Date.parse(observedAt);
  if (Number.isNaN(observedMs) || Number.isNaN(now.getTime())) return 'unknown';
  const seconds = Math.floor((now.getTime() - observedMs) / 1000);
  if (seconds < 60) return 'just now';
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return `${Math.floor(hours / 24)} d ago`;
}
